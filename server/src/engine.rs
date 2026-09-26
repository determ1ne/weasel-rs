//! 管理输入会话、Rime 调用与渲染状态，并在工作线程中串行处理它们。
//!
//! 会话及其原生 Rime 资源只由引擎工作线程访问；来自客户端和渲染器的事件都先进入
//! 工作队列，再由该线程完成状态变更。引擎据上下文令牌和修订号拒绝过期输入与界面事件。
use crate::client_connection::ClientConnection;
use crate::{
    librime,
    renderer_bridge::{RendererPublisher, render_snapshot},
    session_route,
    worker::Processor,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use weasel_common::message::{
    ContextToken, Failure, FailureCode, InputOpened, ModeIndicator, ModeIndicatorReason,
};
use weasel_common::{
    message::{
        Envelope, KeyEventResponse, RenderRect, RenderSnapshot, RendererEvent, envelope::Payload,
    },
    process::RuntimePaths,
};

/// 引擎工作队列的有界容量，用于限制待处理事件占用的内存。
pub(crate) const QUEUE_CAPACITY: usize = 128;

/// 等待 TIP 返回模式提示插入点的最长时间。
///
/// 该期限只防止迟到的布局结果重新唤起旧提示；实际显示时长由 Renderer
/// 根据用户配置从收到新提示时开始计算。
const MODE_INDICATOR_LAYOUT_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// 全局配置决定哪些模式变化需要显示短暂提示。
enum InputModeIndicatorPolicy {
    /// 获得可编辑焦点及用户主动切换时显示。
    FocusAndSwitch,
    /// 仅在用户主动切换且最终模式确实变化时显示。
    SwitchOnly,
    /// 从不显示。
    Never,
}

impl InputModeIndicatorPolicy {
    /// 从配置快照读取策略；非法值按发布默认值 `switch_only` 回退。
    fn from_settings(settings: Option<&weasel_common::settings::ConfigSnapshot>) -> Self {
        let Some(settings) = settings else {
            return Self::SwitchOnly;
        };
        match settings.required::<String>(".input_mode_indicator") {
            Ok(value) if value == "focus_and_switch" => Self::FocusAndSwitch,
            Ok(value) if value == "switch_only" => Self::SwitchOnly,
            Ok(value) if value == "never" => Self::Never,
            Ok(value) => {
                tracing::error!(
                    value,
                    fallback = "switch_only",
                    "invalid input_mode_indicator; using fallback"
                );
                Self::SwitchOnly
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    fallback = "switch_only",
                    "invalid input_mode_indicator; using fallback"
                );
                Self::SwitchOnly
            }
        }
    }

    /// 判断指定来源是否应创建模式提示。
    fn allows(self, reason: ModeIndicatorReason) -> bool {
        match self {
            Self::FocusAndSwitch => true,
            Self::SwitchOnly => reason == ModeIndicatorReason::UserSwitch,
            Self::Never => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// 等待 TIP 返回插入点位置的模式提示请求。
struct PendingModeIndicator {
    /// 与布局回传匹配的不透明标识。
    id: u64,
    /// 已由 Rime 确认的最终模式。
    ascii_mode: bool,
    /// 触发来源，决定主题可采用的外观。
    reason: ModeIndicatorReason,
    /// 服务端接受布局结果的最后时刻。
    expires_at: Instant,
}

/// 工作线程可处理的消息类型。
pub(crate) enum Work {
    /// 客户端请求及其连接存活标记；请求租约在处理完成前阻止连接被回收。
    Message {
        /// 连接的服务端身份。
        client_id: u64,
        /// 承载请求和响应的连接。
        connection: Arc<ClientConnection>,
        /// 连接关闭时变为 `false`，用于丢弃已排队但尚未执行的请求。
        alive: Arc<AtomicBool>,
        /// 解码后的 RPC 信封。
        envelope: Envelope,
        /// 请求处理期间持有的租约；处理后才释放连接回收保护。
        _request: weasel_common::rpc::RequestLease,
    },
    /// 渲染器发来的候选项或其他界面操作。
    Renderer(RendererEvent),
}

/// 某条输入连接在引擎中的原生会话及其路由、渲染快照。
struct ClientSession {
    /// 是否由渲染器显示预编辑文本，而非写入宿主输入框。
    inline_preedit: bool,
    /// 所属客户端连接的身份。
    connection_id: u64,
    /// 响应和布局更新所使用的连接。
    connection: Arc<ClientConnection>,
    /// 连接存活标记；关闭后会话在引擎空闲处理时回收。
    alive: Arc<AtomicBool>,
    /// 仅在引擎工作线程上调用的 Rime 会话。
    session: librime::RimeSession,
    /// 会话状态版本；状态变化时递增，用于拒绝过期渲染操作。
    revision: u64,
    /// 绑定输入上下文令牌并跟踪焦点状态的路由。
    route: session_route::SessionRoute,
    /// 最近一次匹配当前输入上下文的宿主光标矩形。
    anchor: RenderRect,
    /// 新组合文本等待宿主布局确认的最低修订号。
    waiting_for_layout: Option<u64>,
    /// 已接受的最新布局修订号，阻止旧几何覆盖新几何。
    latest_layout_revision: Option<u64>,
    /// 等待 TIP 返回插入点位置的短暂模式提示。
    pending_mode_indicator: Option<PendingModeIndicator>,
    /// 用于增量更新及连接能力切换的最近一次输入状态响应。
    last_response: KeyEventResponse,
}

/// 运行 Rime 输入会话并协调客户端、宿主布局与渲染器。
///
/// 所有实例状态（包括原生会话）由工作线程独占访问；`clients` 先于 `rime` 声明，
/// 以保证会话在 Rime 引擎析构前销毁。数据目录锁还必须存活到 Rime 的终结回调结束。
pub(crate) struct Engine {
    // None 保留每应用行为；Some 为服务生命周期内的共享模式。
    /// `Some` 表示服务内共享的 ASCII 状态；`None` 表示各应用独立处理。
    global_ascii: Option<bool>,
    /// 是否允许在安全输入框中使用 Rime，由配置快照初始化。
    allow_rime_in_secure_fields: bool,
    /// 哪些输入模式变化应请求主题显示提示。
    input_mode_indicator: InputModeIndicatorPolicy,
    /// 可选配置快照；缺省时使用每应用默认行为。
    settings: Option<weasel_common::settings::ConfigSnapshot>,
    // Field order is intentional: destroy every session before dropping the engine.
    /// 以引擎内单调递增的会话标识索引活动输入会话。
    clients: HashMap<u64, ClientSession>,
    /// 原生 Rime 所有者，必须晚于 `clients` 析构。
    rime: librime::Librime,
    // Must outlive the Rime owner, including its finalize callback.
    /// 保持用户数据目录独占，生命周期覆盖 Rime 的初始化和终结。
    _data_lock: crate::data_lock::DataLock,
    /// 当前向渲染器展示的会话标识。
    active_client: Option<u64>,
    /// 向渲染线程发布快照及查询其预编辑文本能力。
    renderer: RendererPublisher,
    /// 下一次分配的会话标识；溢出时拒绝继续分配并触发断言。
    next_session: u64,
    /// 下一次分配的模式提示标识；零保留为“无请求”。
    next_mode_indicator: u64,
}

/// 分配非零且不回绕的模式提示标识。
fn allocate_mode_indicator_id(next: &mut u64) -> u64 {
    let id = *next;
    *next = next
        .checked_add(1)
        .expect("mode indicator identity exhausted");
    id
}

/// 为需要定位的提示建立单槽请求，并把请求标识写入 TIP 响应。
fn request_mode_indicator(
    client: &mut ClientSession,
    response: &mut KeyEventResponse,
    next_id: &mut u64,
    policy: InputModeIndicatorPolicy,
    reason: ModeIndicatorReason,
    ascii_mode: bool,
) {
    if !policy.allows(reason) || response.composing {
        return;
    }
    let id = allocate_mode_indicator_id(next_id);
    client.pending_mode_indicator = Some(PendingModeIndicator {
        id,
        ascii_mode,
        reason,
        expires_at: Instant::now() + MODE_INDICATOR_LAYOUT_TIMEOUT,
    });
    response.mode_indicator_request_id = Some(id);
}

/// 为宿主生成响应副本；外置预编辑时保留提交和组合状态，但清空宿主组合范围。
fn host_response(mut response: KeyEventResponse) -> KeyEventResponse {
    if response.external_preedit {
        // Preserve composing/commit semantics while keeping the host range empty.
        response.composition.clear();
        response.composition_cursor = 0;
    }
    response
}

/// 新组合开始且 Rime 状态确实改变时，使旧光标锚点失效。
///
/// 返回 `true` 表示调用方必须等待本次组合对应的布局，避免用旧位置展示新候选项。
fn reset_anchor_for_new_composition(
    previous: &KeyEventResponse,
    next: &KeyEventResponse,
    anchor: &mut RenderRect,
) -> bool {
    if !previous.composing && next.composing && next.state_updated {
        *anchor = RenderRect::default();
        return true;
    }
    false
}

/// 判断布局更新能否满足当前等待条件且不会回退已接受的布局版本。
///
/// 未携带修订号的旧版 TIP 保持令牌匹配语义；携带修订号时，两项最低版本约束都必须满足。
fn layout_matches_pending(
    revision: Option<u64>,
    waiting_for_layout: Option<u64>,
    latest_layout_revision: Option<u64>,
) -> bool {
    // Older TIPs omit the revision and retain the token-only behavior. A new
    // TIP must not let an in-flight probe from a previous edit unlock this one.
    revision.is_none_or(|revision| {
        waiting_for_layout.is_none_or(|required| revision >= required)
            && latest_layout_revision.is_none_or(|accepted| revision >= accepted)
    })
}

#[cfg(test)]
mod preedit_tests {
    use super::*;

    #[test]
    fn new_composition_waits_for_its_own_layout() {
        let old = KeyEventResponse::default();
        let next = KeyEventResponse {
            composing: true,
            state_updated: true,
            external_preedit: true,
            composition: "nihao".into(),
            ..Default::default()
        };
        let mut anchor = RenderRect {
            left: 10,
            right: 11,
            top: 20,
            bottom: 40,
            valid: true,
        };
        assert!(reset_anchor_for_new_composition(&old, &next, &mut anchor));
        assert!(!anchor.valid);
        assert!(!render_snapshot(1, 1, &next, &anchor).visible);
        anchor.valid = true;
        assert!(!reset_anchor_for_new_composition(&next, &next, &mut anchor));
        assert!(anchor.valid);
        assert!(render_snapshot(1, 1, &next, &anchor).visible);
    }

    #[test]
    fn layout_revision_rejects_old_probes_but_accepts_legacy_tip() {
        assert!(!layout_matches_pending(Some(0), Some(5), Some(3)));
        assert!(!layout_matches_pending(Some(4), Some(5), Some(3)));
        assert!(!layout_matches_pending(Some(2), None, Some(3)));
        assert!(layout_matches_pending(Some(5), Some(5), Some(3)));
        assert!(layout_matches_pending(None, Some(5), Some(3)));
    }

    #[test]
    fn external_preedit_keeps_host_composition_and_commit_but_routes_text_to_renderer() {
        let response = KeyEventResponse {
            external_preedit: true,
            composing: true,
            state_updated: true,
            composition: "nihao".into(),
            composition_cursor: 2,
            commit_text: "前文".into(),
            ..Default::default()
        };
        let anchor = RenderRect {
            left: 10,
            right: 10,
            top: 20,
            bottom: 40,
            valid: true,
        };
        let snapshot = render_snapshot(1, 2, &response, &anchor);
        assert!(snapshot.visible); // Input field remains visible without candidates.
        assert_eq!(snapshot.preedit.unwrap().text, "nihao");
        let host = host_response(response.clone());
        assert!(host.composition.is_empty());
        assert_eq!(host.composition_cursor, 0);
        assert!(host.composing && host.state_updated);
        assert_eq!(host.commit_text, "前文");
        let inline = KeyEventResponse {
            external_preedit: false,
            ..response
        };
        assert_eq!(host_response(inline.clone()), inline);
        assert!(render_snapshot(1, 3, &inline, &anchor).preedit.is_none());
    }
}

/// 将按键响应写回客户端，并附带服务端安全输入策略。
///
/// 队列已关闭等发送错误只记调试日志，不回滚已经完成的引擎状态变更。
fn reply(
    connection: &ClientConnection,
    request_id: u64,
    mut response: KeyEventResponse,
    allow_rime_in_secure_fields: bool,
) {
    response.allow_rime_in_secure_fields = Some(allow_rime_in_secure_fields);
    if let Err(error) = connection.enqueue(Envelope {
        request_id,
        payload: Some(Payload::KeyEventResponse(host_response(response))),
    }) {
        tracing::debug!(%error, "client response enqueue failed");
    }
}

/// 将协议失败写入连接的发送队列；发送失败不影响引擎状态。
fn failure(connection: &ClientConnection, request_id: u64, code: FailureCode, message: &str) {
    let _ = connection.enqueue(Envelope {
        request_id,
        payload: Some(Payload::Failure(Failure {
            code: code as i32,
            message: message.into(),
        })),
    });
}

/// 输入上下文令牌必须包含非零上下文、连接代次和上下文代次。
fn valid_token(token: Option<&ContextToken>) -> bool {
    token.is_some_and(|token| {
        token.context_id != 0 && token.connection_epoch != 0 && token.generation != 0
    })
}

/// 读取 server 使用的布尔配置；快照异常时记录错误并采用调用方给出的安全默认值。
fn bool_setting_or(
    settings: &weasel_common::settings::ConfigSnapshot,
    path: &str,
    fallback: bool,
) -> bool {
    settings.required::<bool>(path).unwrap_or_else(|error| {
        tracing::error!(%error, path, fallback, "invalid server setting; using fallback");
        fallback
    })
}

impl Engine {
    /// 锁定用户数据目录、初始化日志与 Rime，并读取服务级配置。
    ///
    /// 初始化或必需配置读取失败时返回错误；调用者应在专用工作线程中构造实例，
    /// 使后续原生 Rime 操作与初始化处于同一线程。
    pub fn new(
        paths: RuntimePaths,
        renderer: RendererPublisher,
        settings: Option<weasel_common::settings::ConfigSnapshot>,
    ) -> Result<Self, String> {
        let data_lock = crate::data_lock::DataLock::acquire(&paths.user_data)?;
        crate::init_logging(&paths, "server")?;
        let rime = librime::Librime::load(&paths.executable_directory, &paths.user_data)?;
        tracing::info!("librime initialized on engine thread");
        let input_mode_indicator = InputModeIndicatorPolicy::from_settings(settings.as_ref());
        let (global_ascii, allow_rime_in_secure_fields) = match settings.as_ref() {
            Some(settings) => {
                let global_ascii = bool_setting_or(settings, ".global_ascii_status", false)
                    .then(|| bool_setting_or(settings, ".ascii_mode", false));
                let allow_rime_in_secure_fields =
                    bool_setting_or(settings, ".allow_rime_in_secure_fields", false);
                (global_ascii, allow_rime_in_secure_fields)
            }
            None => (None, false),
        };
        Ok(Self {
            global_ascii,
            allow_rime_in_secure_fields,
            input_mode_indicator,
            settings,
            clients: HashMap::new(),
            rime,
            _data_lock: data_lock,
            active_client: None,
            renderer,
            next_session: 1,
            next_mode_indicator: 1,
        })
    }

    /// 校验并处理一个客户端信封，必要时创建、路由或销毁输入会话。
    ///
    /// 过期连接、缺失或无效令牌、未打开的上下文以及跨上下文操作均以协议失败拒绝。
    /// 成功处理会更新会话修订号，并分别向客户端和渲染器发布结果；原生会话操作不得并发。
    fn message(
        &mut self,
        client_id: u64,
        connection: Arc<ClientConnection>,
        alive: Arc<AtomicBool>,
        envelope: Envelope,
    ) {
        if !alive.load(Ordering::Acquire) {
            return;
        }
        let connection_id = client_id;
        let context = match envelope.payload.as_ref() {
            Some(Payload::OpenInput(v)) => v.token.as_ref(),
            Some(Payload::KeyEvent(v)) => v.token.as_ref(),
            Some(Payload::ContextCommand(v)) => v.token.as_ref(),
            _ => None,
        };
        if !valid_token(context) {
            failure(
                &connection,
                envelope.request_id,
                FailureCode::InvalidArgument,
                "input token required",
            );
            return;
        }
        let context_id = context.unwrap().context_id;
        let existing = self.clients.iter().find_map(|(id, client)| {
            (client.connection_id == connection_id
                && client
                    .route
                    .token
                    .as_ref()
                    .is_some_and(|t| t.context_id == context_id))
            .then_some(*id)
        });
        let client_id = match existing {
            Some(id) => id,
            None if matches!(envelope.payload, Some(Payload::OpenInput(_))) => {
                if self
                    .clients
                    .values()
                    .filter(|c| c.connection_id == connection_id)
                    .count()
                    >= 32
                {
                    failure(
                        &connection,
                        envelope.request_id,
                        FailureCode::InvalidArgument,
                        "too many input contexts",
                    );
                    return;
                }
                let id = self.next_session;
                self.next_session = self
                    .next_session
                    .checked_add(1)
                    .expect("session identity exhausted");
                id
            }
            None => {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "OpenInput required before input",
                );
                return;
            }
        };
        if let Some(Payload::ContextCommand(command)) = envelope.payload.as_ref()
            && command.action == weasel_common::message::ContextAction::Destroy as i32
        {
            if let Some(client) = self.clients.get_mut(&client_id)
                && client.route.observe(command.token.as_ref())
            {
                let response = KeyEventResponse {
                    token: command.token,
                    revision: client.revision + 1,
                    ..Default::default()
                };
                reply(
                    &connection,
                    envelope.request_id,
                    response,
                    self.allow_rime_in_secure_fields,
                );
                if self.active_client == Some(client_id) {
                    self.renderer.publish(RenderSnapshot {
                        session_id: client_id,
                        token: command.token,
                        revision: client.revision + 1,
                        ..Default::default()
                    });
                    self.active_client = None;
                }
                self.clients.remove(&client_id);
                connection.forget_layout(context_id);
                connection.set_input_state(
                    self.clients
                        .values()
                        .any(|c| c.connection_id == connection_id && c.route.focused),
                    self.clients
                        .values()
                        .any(|c| c.connection_id == connection_id && c.last_response.composing),
                );
            } else {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::StaleContext,
                    "stale destroy context",
                );
            }
            return;
        }
        // Only explicit OpenInput allocates a native session. Reopening is idempotent
        // for the same token; context changes must use ContextCommand.
        if let Some(Payload::OpenInput(open)) = envelope.payload.as_ref() {
            if !valid_token(open.token.as_ref()) {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "nonzero input token required",
                );
                return;
            }
            if let Some(client) = self.clients.get(&client_id) {
                if client.route.token != open.token {
                    failure(
                        &connection,
                        envelope.request_id,
                        FailureCode::InvalidArgument,
                        "input already opened; use context commands",
                    );
                    return;
                }
            } else {
                let mut session = match self.rime.new_session() {
                    Ok(session) => session,
                    Err(error) => {
                        tracing::error!(client_id, %error, "session creation failed");
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::Internal,
                            "session creation failed",
                        );
                        return;
                    }
                };
                if let Some(ascii) = self.global_ascii.or_else(|| {
                    self.settings.as_ref().and_then(|settings| {
                        settings
                            .app_bool(connection.client_executable().unwrap_or(""), "ascii_mode")
                            .map_err(|error| {
                                tracing::error!(%error, "invalid application setting");
                                error
                            })
                            .ok()
                    })
                }) {
                    session.set_ascii_mode(ascii);
                }
                self.clients.insert(
                    client_id,
                    ClientSession {
                        inline_preedit: self.settings.as_ref().is_none_or(|settings| {
                            settings
                                .app_bool(
                                    connection.client_executable().unwrap_or(""),
                                    "inline_preedit",
                                )
                                .unwrap_or_else(|error| {
                                    tracing::error!(%error, "invalid application setting");
                                    true
                                })
                        }),
                        connection_id,
                        connection: connection.clone(),
                        alive,
                        session,
                        revision: 0,
                        route: session_route::SessionRoute {
                            token: open.token.clone(),
                            focused: false,
                        },
                        anchor: Default::default(),
                        waiting_for_layout: None,
                        latest_layout_revision: None,
                        pending_mode_indicator: None,
                        last_response: Default::default(),
                    },
                );
            }
            let _ = connection.enqueue(Envelope {
                request_id: envelope.request_id,
                payload: Some(Payload::InputOpened(InputOpened {
                    token: open.token.clone(),
                })),
            });
            return;
        }
        if !self.clients.contains_key(&client_id) {
            if matches!(
                envelope.payload,
                Some(Payload::KeyEvent(_) | Payload::ContextCommand(_))
            ) {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "OpenInput required before input",
                );
            }
            return;
        }
        let token = match envelope.payload.as_ref() {
            Some(Payload::KeyEvent(key)) => Some(key.token.as_ref()),
            Some(Payload::ContextCommand(command)) => Some(command.token.as_ref()),
            _ => None,
        };
        if token.is_some_and(|token| !valid_token(token)) {
            failure(
                &connection,
                envelope.request_id,
                FailureCode::InvalidArgument,
                "nonzero input token required",
            );
            return;
        }
        match envelope.payload {
            Some(Payload::KeyEvent(key_event)) => {
                let (response, snapshot) = {
                    let next_mode_indicator = &mut self.next_mode_indicator;
                    let client = self
                        .clients
                        .get_mut(&client_id)
                        .expect("client session exists");
                    if !client.route.observe(key_event.token.as_ref()) {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::StaleContext,
                            "stale or mismatched key context",
                        );
                        return;
                    } else {
                        let focus_changed = self.active_client != Some(client_id);
                        self.active_client = Some(client_id);
                        client.route.focused = true;
                        if let Some(ascii) = self.global_ascii {
                            client.session.set_ascii_mode(ascii);
                        }
                        let mut response = client.session.process_key(&key_event);
                        if self.global_ascii.is_some()
                            && let Some(ascii) = response.ascii_mode
                        {
                            self.global_ascii = Some(ascii);
                        }
                        response.external_preedit = response.composing
                            && self.renderer.supports_preedit()
                            && !client.inline_preedit;
                        let previous_visible = client.anchor.valid
                            && (!client.last_response.candidates.is_empty()
                                || (client.last_response.external_preedit
                                    && client.last_response.composing));
                        let previous_ascii = client.last_response.ascii_mode;
                        let anchor_reset = reset_anchor_for_new_composition(
                            &client.last_response,
                            &response,
                            &mut client.anchor,
                        );
                        if response.state_updated {
                            client.revision = client.revision.wrapping_add(1);
                        }
                        response.token = client.route.token.clone();
                        response.revision = client.revision;
                        if anchor_reset {
                            client.waiting_for_layout = Some(response.revision);
                        } else if response.state_updated && !response.composing {
                            client.waiting_for_layout = None;
                        }
                        if response.state_updated {
                            client.last_response = response.clone();
                            client.last_response.mode_indicator_request_id = None;
                        }
                        if response.ascii_mode.is_some() {
                            client.last_response.ascii_mode = response.ascii_mode;
                        }
                        client.last_response.token = client.route.token.clone();
                        if response.composing {
                            client.pending_mode_indicator = None;
                        } else if previous_ascii
                            .zip(response.ascii_mode)
                            .is_some_and(|(previous, next)| previous != next)
                        {
                            let ascii_mode = response.ascii_mode.expect("checked above");
                            request_mode_indicator(
                                client,
                                &mut response,
                                next_mode_indicator,
                                self.input_mode_indicator,
                                ModeIndicatorReason::UserSwitch,
                                ascii_mode,
                            );
                        }
                        let snapshot = if client.waiting_for_layout.is_some() {
                            // The host edit has not supplied geometry for this
                            // composition. Do not send an intermediate frame
                            // containing new candidates but no matching anchor.
                            (previous_visible
                                || focus_changed
                                || previous_ascii != client.last_response.ascii_mode)
                                .then(|| RenderSnapshot {
                                    session_id: client_id,
                                    revision: client.revision,
                                    token: client.route.token.clone(),
                                    active: true,
                                    ascii_mode: client.last_response.ascii_mode,
                                    ..Default::default()
                                })
                        } else {
                            (response.state_updated || focus_changed).then(|| {
                                render_snapshot(
                                    client_id,
                                    client.revision,
                                    &client.last_response,
                                    &client.anchor,
                                )
                            })
                        };
                        (response, snapshot)
                    }
                };
                reply(
                    &connection,
                    envelope.request_id,
                    response,
                    self.allow_rime_in_secure_fields,
                );
                if let Some(snapshot) = snapshot {
                    self.renderer.publish(snapshot);
                }
            }
            Some(Payload::ContextCommand(command)) => {
                use weasel_common::message::ContextAction;
                let (response, snapshot) = {
                    let next_mode_indicator = &mut self.next_mode_indicator;
                    let client = self
                        .clients
                        .get_mut(&client_id)
                        .expect("client session exists");
                    let action = ContextAction::try_from(command.action)
                        .unwrap_or(ContextAction::Unspecified);
                    if action == ContextAction::Unspecified {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::InvalidArgument,
                            "unknown context action",
                        );
                        return;
                    }
                    let previous_context =
                        client.route.token.as_ref().map(|token| token.context_id);
                    if !client.route.observe_command(command.token.as_ref(), action) {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::StaleContext,
                            "stale or mismatched command context",
                        );
                        return;
                    } else {
                        if previous_context
                            != client.route.token.as_ref().map(|token| token.context_id)
                        {
                            client.session.context_action(ContextAction::Cancel);
                            client.last_response = Default::default();
                            client.anchor = Default::default();
                            client.waiting_for_layout = None;
                            client.latest_layout_revision = None;
                            client.pending_mode_indicator = None;
                        }
                        let was_active = self.active_client == Some(client_id);
                        match action {
                            ContextAction::Focus => {
                                client.route.focused = true;
                                self.active_client = Some(client_id);
                            }
                            ContextAction::Blur | ContextAction::HostTerminated => {
                                client.route.focused = false;
                                if was_active {
                                    self.active_client = None;
                                }
                            }
                            _ => {}
                        }
                        // SetAscii 是旧 TIP 重连时恢复本地记忆的握手，仍按原值确认，
                        // 但不写共享状态；随后的 Focus 再恢复共享值，兼容现有 TIP。
                        if matches!(action, ContextAction::Focus | ContextAction::ToggleAscii)
                            && let Some(ascii) = self.global_ascii
                        {
                            client.session.set_ascii_mode(ascii);
                        }
                        let previous_ascii = client.last_response.ascii_mode;
                        let mut response = match (action, command.ascii_mode) {
                            (ContextAction::SetAscii, None) => {
                                failure(
                                    &connection,
                                    envelope.request_id,
                                    FailureCode::InvalidArgument,
                                    "SetAscii requires ascii_mode",
                                );
                                return;
                            }
                            (ContextAction::SetAscii, Some(ascii)) => {
                                client.session.set_ascii_mode_response(ascii)
                            }
                            _ => client.session.context_action(action),
                        };
                        if action == ContextAction::ToggleAscii
                            && self.global_ascii.is_some()
                            && let Some(ascii) = response.ascii_mode
                        {
                            self.global_ascii = Some(ascii);
                        }
                        response.external_preedit = response.composing
                            && self.renderer.supports_preedit()
                            && !client.inline_preedit;
                        client.revision = client.revision.wrapping_add(1);
                        response.token = client.route.token.clone();
                        response.revision = client.revision;
                        if matches!(
                            action,
                            ContextAction::Blur
                                | ContextAction::Cancel
                                | ContextAction::Submit
                                | ContextAction::HostTerminated
                        ) || (response.state_updated && !response.composing)
                        {
                            client.waiting_for_layout = None;
                        }
                        if matches!(
                            action,
                            ContextAction::Blur
                                | ContextAction::Cancel
                                | ContextAction::Submit
                                | ContextAction::HostTerminated
                        ) || response.composing
                        {
                            client.pending_mode_indicator = None;
                        }
                        if response.state_updated {
                            client.last_response = response.clone();
                            client.last_response.mode_indicator_request_id = None;
                        }
                        if response.ascii_mode.is_some() {
                            client.last_response.ascii_mode = response.ascii_mode;
                        }
                        // Focus acknowledgements must not replay the previous commit.
                        client.last_response.token = client.route.token.clone();
                        let indicator = match action {
                            ContextAction::Focus => response
                                .ascii_mode
                                .map(|ascii| (ModeIndicatorReason::Focus, ascii)),
                            ContextAction::ToggleAscii => previous_ascii
                                .zip(response.ascii_mode)
                                .filter(|(previous, next)| previous != next)
                                .map(|(_, ascii)| (ModeIndicatorReason::UserSwitch, ascii)),
                            _ => None,
                        };
                        if let Some((reason, ascii_mode)) = indicator {
                            request_mode_indicator(
                                client,
                                &mut response,
                                next_mode_indicator,
                                self.input_mode_indicator,
                                reason,
                                ascii_mode,
                            );
                        }
                        let snapshot = if self.active_client == Some(client_id) {
                            Some(render_snapshot(
                                client_id,
                                client.revision,
                                &client.last_response,
                                &client.anchor,
                            ))
                        } else if was_active {
                            Some(RenderSnapshot {
                                session_id: client_id,
                                revision: client.revision,
                                token: client.route.token.clone(),
                                ..Default::default()
                            })
                        } else {
                            None
                        };
                        (response, snapshot)
                    }
                };
                reply(
                    &connection,
                    envelope.request_id,
                    response,
                    self.allow_rime_in_secure_fields,
                );
                if let Some(snapshot) = snapshot {
                    self.renderer.publish(snapshot);
                }
            }
            _ => (),
        }
    }

    /// 仅将匹配当前活动会话令牌和修订号的渲染器操作交给 Rime。
    ///
    /// 断开连接、失焦或版本过期的事件会被忽略；有效操作递增会话修订号并发布新状态。
    fn renderer_event(&mut self, event: RendererEvent) {
        let Some(client) = self.clients.get_mut(&event.session_id) else {
            tracing::debug!(
                session_id = event.session_id,
                "ignored renderer event for disconnected client"
            );
            return;
        };
        if self.active_client != Some(event.session_id)
            || !client.alive.load(Ordering::Acquire)
            || !client.route.accepts_ui(&event, client.revision)
        {
            return;
        }
        let mut response = client.session.process_renderer_event(&event);
        response.external_preedit =
            response.composing && self.renderer.supports_preedit() && !client.inline_preedit;
        client.revision = client.revision.wrapping_add(1);
        response.token = client.route.token.clone();
        response.revision = client.revision;
        if !response.composing {
            client.waiting_for_layout = None;
        }
        client.last_response = response.clone();
        let snapshot =
            render_snapshot(event.session_id, client.revision, &response, &client.anchor);
        let connection = Arc::clone(&client.connection);
        reply(&connection, 0, response, self.allow_rime_in_secure_fields);
        self.renderer.publish(snapshot);
    }
}

impl Processor<Work> for Engine {
    /// 在工作线程串行分派客户端消息和渲染器事件。
    ///
    /// 消息处理后先重算连接的输入活跃度，再释放请求租约，以免回收器观察到过期状态。
    fn process(&mut self, work: Work) {
        match work {
            Work::Message {
                client_id,
                connection,
                alive,
                envelope,
                _request,
            } => {
                self.message(client_id, connection, alive, envelope);
                // Reclassify input before dropping the request's eviction guard.
                self.idle();
                drop(_request);
            }
            Work::Renderer(event) => self.renderer_event(event),
        }
    }

    /// 处理能力变化、最新宿主布局和断连会话的清理。
    ///
    /// 每轮仅消费活动上下文的最新布局；版本不匹配的几何不会解除新组合的等待状态。
    fn idle(&mut self) {
        // Capability changes wake this worker. Restore inline text immediately
        // after a renderer disconnect; do not wait for the next keystroke.
        if !self.renderer.supports_preedit() {
            for (id, client) in &mut self.clients {
                if client.last_response.external_preedit && client.alive.load(Ordering::Acquire) {
                    client.last_response.external_preedit = false;
                    client.revision = client.revision.wrapping_add(1);
                    client.last_response.revision = client.revision;
                    let mut response = client.last_response.clone();
                    response.commit_text.clear();
                    response.open_emoji_panel = false;
                    response.state_updated = true;
                    response.eaten = false;
                    reply(
                        &client.connection,
                        0,
                        response,
                        self.allow_rime_in_secure_fields,
                    );
                    if self.active_client == Some(*id) {
                        self.renderer.publish(render_snapshot(
                            *id,
                            client.revision,
                            &client.last_response,
                            &client.anchor,
                        ));
                    }
                }
            }
        }
        // Poll the active connection's latest geometry, never the input FIFO.
        // Layout arrival explicitly wakes the engine, including the final drag position.
        if let Some(client_id) = self.active_client
            && let Some(client) = self.clients.get_mut(&client_id)
            && client.alive.load(Ordering::Acquire)
            && let Some(update) = client
                .connection
                .take_layout_for(client.route.token.as_ref())
        {
            if let Some(request_id) = update.mode_indicator_request_id {
                if let Some(pending) = client
                    .pending_mode_indicator
                    .filter(|pending| pending.id == request_id)
                {
                    client.pending_mode_indicator = None;
                    let now = Instant::now();
                    let anchor = update.anchor.unwrap_or_default();
                    if now < pending.expires_at && anchor.valid && !client.last_response.composing {
                        let mut snapshot = render_snapshot(
                            client_id,
                            client.revision,
                            &client.last_response,
                            &anchor,
                        );
                        snapshot.mode_indicator = Some(ModeIndicator {
                            id: pending.id,
                            ascii_mode: pending.ascii_mode,
                            reason: pending.reason as i32,
                        });
                        self.renderer.publish(snapshot);
                    }
                }
            } else {
                let matching = layout_matches_pending(
                    update.revision,
                    client.waiting_for_layout,
                    client.latest_layout_revision,
                );
                if matching {
                    if let Some(revision) = update.revision {
                        client.latest_layout_revision = Some(revision);
                    }
                    let anchor = update.anchor.unwrap_or_default();
                    let became_ready = client.waiting_for_layout.is_some() && anchor.valid;
                    if became_ready {
                        client.waiting_for_layout = None;
                    }
                    if client.anchor != anchor || became_ready {
                        client.anchor = anchor;
                        if client.waiting_for_layout.is_none()
                            && (!client.last_response.candidates.is_empty()
                                || client.last_response.external_preedit)
                        {
                            self.renderer.publish(render_snapshot(
                                client_id,
                                client.revision,
                                &client.last_response,
                                &client.anchor,
                            ));
                        }
                    }
                }
            }
        }
        let hide = self.active_client.and_then(|client_id| {
            self.clients
                .get(&client_id)
                .filter(|client| !client.alive.load(Ordering::Acquire))
                .map(|client| RenderSnapshot {
                    session_id: client_id,
                    revision: client.revision.wrapping_add(1),
                    token: client.route.token.clone(),
                    ..Default::default()
                })
        });
        self.clients
            .retain(|_, client| client.alive.load(Ordering::Acquire));
        let mut connections = HashMap::new();
        for client in self.clients.values() {
            let entry = connections.entry(client.connection_id).or_insert((
                &client.connection,
                false,
                false,
            ));
            entry.1 |= client.route.focused;
            entry.2 |= client.last_response.composing;
        }
        for (connection, focused, composing) in connections.values() {
            connection.set_input_state(*focused, *composing);
        }
        if let Some(client_id) = self.active_client {
            if !self.clients.contains_key(&client_id) {
                self.active_client = None;
                if let Some(hide) = hide {
                    self.renderer.publish(hide);
                }
            }
        }
    }
}
