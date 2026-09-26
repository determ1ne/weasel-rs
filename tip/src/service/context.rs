//! 保存每个 TSF 文本上下文独立的会话、组合与路由状态。
//!
//! 管道响应必须按上下文身份和令牌路由，不能因通知到达时焦点变化而改投
//! 当前焦点。生命周期、连接代次及编辑代次共同阻止迟到响应和重入回调
//! 操作已经移除、挂起或替换的上下文。
use super::*;
use weasel_common::message::{ContextAction, ContextCommand, ContextToken};

static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// 一个 TSF 文本上下文对应的 Rust 状态与 COM 资源集合。
///
/// `alive`、`suspended` 和 `generation` 共同决定上下文能否接收响应；组合、
/// 选择及布局字段只描述此上下文，不能跨上下文复用。访问 COM 对象及互斥
/// 状态时须考虑宿主回调重入，并避免在锁持有期间引入不受控的回调。
pub(super) struct ContextState {
    /// 订阅事件及请求 TSF 编辑会话时使用的上下文 COM 接口。
    pub context: ITfContext,
    /// 用于 COM 身份比较的规范 `IUnknown`，确保同一对象的不同接口视图仍匹配。
    identity: IUnknown,
    /// 本进程内分配的上下文标识，写入发往引擎的上下文令牌。
    pub id: u64,
    /// 上下文是否仍可接受响应；停止时先发布为 `false`。
    pub alive: AtomicBool,
    /// 上下文是否暂时不可路由响应或执行编辑。
    pub suspended: AtomicBool,
    /// 失效代次；焦点相关状态重置、移除或断连清理时使旧令牌失效。
    pub generation: AtomicU64,
    /// 与引擎通信的共享工作线程状态；连接代次参与令牌校验。
    pub rpc: Arc<Mutex<RpcWorker>>,
    /// 当前 TSF 组合对象；终止回调可能与编辑步骤重入交错。
    pub composition: Mutex<Option<ITfComposition>>,
    /// 创建当前组合时的连接代次，用于检测组合所属连接是否已断开。
    pub composition_epoch: AtomicU64,
    /// 最近成功应用到组合的布局响应修订号。
    pub applied_layout_revision: AtomicU64,
    /// 断连组合清理请求是否已排队或正在执行，防止重复安排。
    pub disconnect_requested: AtomicBool,
    /// 最近应用到 TSF 的预编辑显示文本。
    pub composition_text: Mutex<String>,
    /// 当前显示文本对应的提交阶段及可恢复原始输入。
    pub composition_content: Mutex<composition::CompositionContent>,
    /// 原始预编辑文本收尾任务是否正在进行。
    pub finishing_raw: AtomicBool,
    /// 当前组合中的光标位置，以 UTF-16 文本范围的字符偏移表示。
    pub composition_cursor: Mutex<usize>,
    /// 此上下文待处理的布局通知计划。
    pub layout: Mutex<layout::LayoutSchedule>,
    /// 是否正由 TSF 编辑会话修改。
    pub editing: AtomicBool,
    /// 是否正在与引擎重新协调上下文状态。
    pub reconciling: AtomicBool,
    /// 组合开始前暂存的宿主选择及其样式，组合正常结束时可恢复。
    pub host_selection: Mutex<Option<(ITfRange, TF_SELECTIONSTYLE)>>,
    /// 当前连接代次内接受响应的上下文令牌与修订号。
    pub route: Mutex<ResponseRoute>,
    /// 引擎 ASCII 模式及所属连接代次，不仅由文档决定。
    pub input_mode: Mutex<Option<(u64, bool)>>,
    /// 安全输入策略探测结果；取值定义于 `secure_input` 模块。
    pub secure_field: AtomicU8,
    /// 此上下文安全输入属性的探测计划。
    pub secure_probe: Mutex<secure_input::SecureProbeSchedule>,
    /// 当前是否已启用安全输入绕过策略。
    pub secure_bypass_active: AtomicBool,
    /// 在 TSF 事件源注册的 Sink cookie；停止时解除订阅。
    cookies: Mutex<Vec<u32>>,
}

/// 用于筛选上下文响应的单调路由状态。
///
/// 修订号只在同一连接代次内递增；连接代次切换时重置修订号，避免新连接
/// 的响应被旧连接较大的修订号拒绝。
#[derive(Default)]
pub(super) struct ResponseRoute {
    /// 最近接受响应所属的引擎连接代次。
    epoch: u64,
    /// 当前连接代次内最近接受的响应修订号。
    revision: u64,
}

impl ResponseRoute {
    /// 校验响应确实属于预期令牌且修订号严格递增，并在接受时推进路由状态。
    ///
    /// 零连接代次、令牌不匹配以及重复或过期修订号都会被拒绝；遇到新的
    /// 有效连接代次时先清零修订号，再按该代次规则检查响应。
    fn accept(&mut self, expected: &ContextToken, response: &KeyEventResponse) -> bool {
        if response.token.as_ref() != Some(expected) || expected.connection_epoch == 0 {
            return false;
        }
        if self.epoch != expected.connection_epoch {
            self.epoch = expected.connection_epoch;
            self.revision = 0;
        }
        if response.revision <= self.revision {
            return false;
        }
        self.revision = response.revision;
        true
    }
}

impl ContextState {
    /// 从当前 RPC 连接与上下文生命周期状态构造响应令牌。
    ///
    /// RPC 锁正被其他流程持有时返回边界失败，避免为响应生成不一致的连接
    /// 代次；成功令牌同时包含上下文标识和当前失效代次。
    pub fn token(&self) -> Result<ContextToken> {
        let rpc = self
            .rpc
            .try_lock()
            .map_err(|_| Error::from_hresult(boundary::E_FAIL))?;
        Ok(ContextToken {
            context_id: self.id,
            connection_epoch: rpc.connection_epoch(),
            generation: self.generation.load(Ordering::Acquire),
        })
    }

    /// 判断上下文仍可用且给定令牌与此刻的连接和生命周期状态完全匹配。
    pub fn matches(&self, token: Option<&ContextToken>) -> Result<bool> {
        Ok(self.alive.load(Ordering::Acquire)
            && !self.suspended.load(Ordering::Acquire)
            && token == Some(&self.token()?))
    }

    /// 标记上下文停止，并在不处于 TSF 编辑期间时解除事件订阅并释放组合资源。
    ///
    /// 先将 `alive` 置为假，使并发到达的响应失效。若仍在编辑，则推迟释放并
    /// 返回 `false`；否则递增代次、尝试取得可安全清理的资源锁，再调用 TSF
    /// 解除 Sink。调用者应在返回 `false` 时安排维护重试。
    pub fn stop(&self) -> bool {
        self.alive.store(false, Ordering::Release);
        if self.editing.load(Ordering::Acquire) {
            return false;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        let resources = (|| {
            let mut cookies = boundary::try_teardown(&self.cookies)?;
            let mut composition = boundary::try_teardown(&self.composition)?;
            let mut selection = boundary::try_teardown(&self.host_selection)?;
            Some((
                std::mem::take(&mut *cookies),
                composition.take(),
                selection.take(),
            ))
        })();
        let Some((cookies, composition, selection)) = resources else {
            return false;
        };
        if let Ok(source) = self.context.cast::<ITfSource>() {
            for cookie in cookies {
                unsafe {
                    let _ = source.UnadviseSink(cookie);
                }
            }
        }
        drop(composition);
        drop(selection);
        true
    }
}

impl Drop for ContextState {
    /// 最后一道无异常清理保障；TSF 清理错误在对象析构边界内被封装处理。
    fn drop(&mut self) {
        boundary::cleanup(|| {
            self.stop();
        });
    }
}

impl TextService {
    /// 按规范 COM 身份查找已登记的上下文状态。
    pub(super) fn find_context(&self, context: &ITfContext) -> Result<Option<Arc<ContextState>>> {
        let identity: IUnknown = context.cast()?;
        Ok(self
            .lock(&self.contexts)?
            .iter()
            .find(|state| state.identity == identity)
            .cloned())
    }

    /// 查找或创建上下文状态，并向 TSF 注册文本编辑与布局 Sink。
    ///
    /// 上下文数量受限，服务必须处于激活状态。订阅期间若服务代次变化或
    /// 服务停用，会撤销部分初始化并返回错误；只有设置成功后状态才进入
    /// 全局上下文列表。
    pub(super) fn ensure_context(
        &self,
        context: ITfContext,
        owner: &IUnknown,
    ) -> Result<Arc<ContextState>> {
        if let Some(state) = self.find_context(&context)? {
            return Ok(state);
        }
        if !self.activated.load(Ordering::Acquire) || self.lock(&self.contexts)?.len() >= 32 {
            return Err(Error::from_hresult(boundary::E_FAIL));
        }
        let generation = self.generation.load(Ordering::Acquire);
        let state = Arc::new(ContextState {
            identity: context.cast()?,
            context,
            id: NEXT_CONTEXT.fetch_add(1, Ordering::AcqRel),
            alive: AtomicBool::new(true),
            suspended: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            rpc: self.rpc.clone(),
            composition: Mutex::new(None),
            composition_epoch: AtomicU64::new(0),
            applied_layout_revision: AtomicU64::new(0),
            disconnect_requested: AtomicBool::new(false),
            composition_text: Mutex::new(String::new()),
            composition_content: Mutex::new(Default::default()),
            finishing_raw: AtomicBool::new(false),
            composition_cursor: Mutex::new(0),
            layout: Mutex::default(),
            editing: AtomicBool::new(false),
            reconciling: AtomicBool::new(false),
            host_selection: Mutex::new(None),
            route: Mutex::new(ResponseRoute::default()),
            input_mode: Mutex::new(None),
            secure_field: AtomicU8::new(secure_input::UNKNOWN),
            secure_probe: Mutex::default(),
            secure_bypass_active: AtomicBool::new(false),
            cookies: Mutex::new(Vec::new()),
        });
        let source: ITfSource = state.context.cast()?;
        let setup: Result<()> = (|| {
            for iid in [ITfTextEditSink::IID, ITfTextLayoutSink::IID] {
                let cookie = unsafe { source.AdviseSink(&iid, owner)? };
                self.lock(&state.cookies)?.push(cookie);
            }
            let hwnd = self
                .lock(&self.update_window)?
                .as_ref()
                .map(|w| w.hwnd.0 as usize)
                .unwrap_or(0);
            self.lock(&state.rpc)?.set_update_window(hwnd);
            if self.generation.load(Ordering::Acquire) != generation
                || !self.activated.load(Ordering::Acquire)
            {
                return Err(Error::from_hresult(boundary::E_FAIL));
            }
            Ok(())
        })();
        if let Err(error) = setup {
            state.stop();
            return Err(error);
        }
        self.lock(&self.contexts)?.push(state.clone());
        Ok(state)
    }

    /// 切换焦点上下文并向引擎发送旧上下文失焦、新上下文聚焦通知。
    ///
    /// 先更新本地焦点标识并清除已测试按键，再发送上下文动作，最后刷新语言栏。
    /// 引擎通知失败由 `send_context_action` 按上下文状态处理。
    pub(super) fn focus_context(&self, next: Option<Arc<ContextState>>) -> Result<()> {
        let id = next.as_ref().map(|state| state.id);
        let previous = {
            let mut focus = self.lock(&self.focused_context)?;
            if *focus == id {
                return Ok(());
            }
            std::mem::replace(&mut *focus, id)
        };
        self.lock(&self.tested_key)?.take();
        let states = self.lock(&self.contexts)?.clone();
        for state in states {
            if Some(state.id) == previous {
                self.lock(&state.layout)?.cancel_indicator();
                self.send_context_action(&state, ContextAction::Blur, false)?;
            }
        }
        if let Some(state) = next {
            self.send_context_action(&state, ContextAction::Focus, false)?;
        }
        self.refresh_language_bar()?;
        Ok(())
    }

    /// 停止并移除指定上下文，清理其编辑队列并继续调度其他上下文。
    ///
    /// 若上下文仍在 TSF 编辑中，保留状态并请求维护重试。成功停止后向引擎
    /// 发送销毁动作，使正在执行或排队的该上下文编辑失效，再移除其队列项。
    pub(super) fn remove_context(&self, context: &ITfContext) -> Result<()> {
        let Some(state) = self.find_context(context)? else {
            return Ok(());
        };
        let was_focused = *self.lock(&self.focused_context)? == Some(state.id);
        if was_focused {
            self.focus_context(None)?;
        }
        if !state.stop() {
            self.faulted.request_maintenance();
            return Ok(());
        }
        let token = state.token()?;
        let _ = self.lock(&self.rpc)?.context_command(ContextCommand {
            token: Some(token),
            action: ContextAction::Destroy as i32,
            ascii_mode: None,
        });
        let removed = {
            let mut states = self.lock(&self.contexts)?;
            states
                .iter()
                .position(|s| s.id == state.id)
                .map(|index| states.remove(index))
        };
        if self.active_edit_context.load(Ordering::Acquire) == state.id {
            self.edit_ticket.fetch_add(1, Ordering::AcqRel);
            self.edit_requested.store(false, Ordering::Release);
        }
        let discarded = {
            let mut queue = self.lock(&self.pending_edit)?;
            let old = std::mem::take(&mut *queue);
            let (keep, discard): (VecDeque<PendingEdit>, VecDeque<PendingEdit>) = old
                .into_iter()
                .partition(|task: &PendingEdit| task.state.id != state.id);
            *queue = keep;
            discard
        };
        drop(discarded);
        drop(removed);
        self.schedule_edit()?;
        Ok(())
    }

    /// 向引擎发送聚焦、失焦或销毁等上下文转换动作。
    ///
    /// 可选的失效操作会先递增上下文代次并取消关联的编辑预留。失败转换不
    /// 盲目重放：连接代次由工作线程失效；若存在组合或本次转换已使状态失效，
    /// 则隔离该上下文以防旧组合状态继续应用。
    pub(super) fn send_context_action(
        &self,
        state: &ContextState,
        action: ContextAction,
        invalidate: bool,
    ) -> Result<()> {
        if state.suspended.load(Ordering::Acquire) && action != ContextAction::Blur {
            return Ok(());
        }
        if invalidate {
            state.reconciling.store(true, Ordering::Release);
            state.generation.fetch_add(1, Ordering::AcqRel);
            self.lock(&self.tested_key)?.take();
            if self.active_edit_context.load(Ordering::Acquire) == state.id {
                self.edit_ticket.fetch_add(1, Ordering::AcqRel);
                self.edit_requested.store(false, Ordering::Release);
            }
        }
        let command = ContextCommand {
            token: Some(state.token()?),
            action: action as i32,
            ascii_mode: None,
        };
        let result = self.lock(&state.rpc)?.context_command(command);
        if result.is_err() {
            // The worker invalidates its epoch on rejection. A failed context
            // transition is not replayable while a composition may exist.
            if self.lock(&state.composition)?.is_some() || invalidate {
                self.quarantine(state, "context.transition_failed", action as u64);
            }
            return Ok(());
        }
        Ok(())
    }

    /// 收取并分发各上下文的引擎更新，必要时申请 TSF 编辑会话。
    ///
    /// 仅处理仍存活且未挂起的目标；每条响应均按上下文令牌和修订号筛选，
    /// 再同步安全输入、输入模式及组合文本。若某个可失败的 TSF 处理提前退出，
    /// 其他上下文尚未收取的更新仍保留在队列中。
    pub(super) fn drain_context_updates(&self, owner: &IUnknown) -> Result<()> {
        let states = self.lock(&self.contexts)?.clone();
        // Remove dead destinations, but leave other contexts queued if one
        // context's fallible TSF processing exits early during reentrancy.
        let destinations: Vec<_> = states
            .iter()
            .filter(|state| {
                state.alive.load(Ordering::Acquire) && !state.suspended.load(Ordering::Acquire)
            })
            .map(|state| state.id)
            .collect();
        self.lock(&self.rpc)?.retain_context_updates(&destinations);
        for state in states {
            if state.suspended.load(Ordering::Acquire) || !state.alive.load(Ordering::Acquire) {
                continue;
            }
            if self.cleanup_disconnected_composition(&state, owner)? {
                continue;
            }
            let responses = self.lock(&self.rpc)?.take_context_updates(state.id);
            for response in responses {
                let token = state.token()?;
                if !state.alive.load(Ordering::Acquire)
                    || !self.lock(&state.route)?.accept(&token, &response)
                {
                    continue;
                }
                self.update_secure_policy(
                    response.allow_rime_in_secure_fields,
                    token.connection_epoch,
                );
                state.reconciling.store(false, Ordering::Release);
                if let Some(ascii) = response.ascii_mode {
                    *self.lock(&state.input_mode)? = Some((token.connection_epoch, ascii));
                    let is_focused = *self.lock(&self.focused_context)? == Some(state.id);
                    if is_focused {
                        self.remember_input_mode(ascii)?;
                    }
                    self.refresh_language_bar()?;
                }
                if let Some(request_id) = response.mode_indicator_request_id {
                    // 提示定位失败不影响键入，也不应把 TIP 置为故障状态。
                    let _ = self.request_mode_indicator_layout(&state, request_id);
                }
                if response::has_edit_payload(&response) {
                    response::validate(&response)?;
                    let needs_edit = !response.commit_text.is_empty()
                        || response.composing != self.lock(&state.composition)?.is_some()
                        || response.composition != *self.lock(&state.composition_text)?
                        || response.composition_cursor as usize
                            != *self.lock(&state.composition_cursor)?
                        || response.open_emoji_panel
                        || self
                            .lock(&self.pending_edit)?
                            .iter()
                            .any(|task| task.state.id == state.id)
                        || (self.edit_requested.load(Ordering::Acquire)
                            && self.active_edit_context.load(Ordering::Acquire) == state.id);
                    if needs_edit {
                        self.request_edit_session(
                            state.context.clone(),
                            response,
                            EditStep::ApplyResponse,
                            owner.clone(),
                        )?;
                    } else if response.state_updated && response.composing {
                        // No queued/in-flight edit: the raw encoding may change
                        // even when the displayed preedit/caret is unchanged.
                        *self.lock(&state.composition_content)? =
                            composition::CompositionContent::preedit(response.raw_input.clone());
                    }
                }
            }
        }
        Ok(())
    }
}

impl TextService {
    /// 检查组合是否属于已经断开的 RPC 连接，并排队一次 TSF 断连清理。
    ///
    /// 仅当上下文仍有组合且组合创建代次不同于当前连接代次时清理。代次递增
    /// 使旧响应失效，已排队编辑会被丢弃；清理步骤不依赖旧响应令牌。返回值
    /// 表示组合是否仍存在，供调用者决定是否继续处理该上下文更新。
    pub(super) fn cleanup_disconnected_composition(
        &self,
        state: &Arc<ContextState>,
        owner: &IUnknown,
    ) -> Result<bool> {
        let epoch = state.token()?.connection_epoch;
        if self.lock(&state.composition)?.is_none() {
            return Ok(false);
        }
        if state.composition_epoch.load(Ordering::Acquire) == epoch {
            return Ok(false);
        }
        if state.disconnect_requested.swap(true, Ordering::AcqRel) {
            return Ok(true);
        }
        state.generation.fetch_add(1, Ordering::AcqRel);
        let result = (|| {
            self.lock(&self.tested_key)?.take();
            self.discard_context_edits(state.id)?;
            let response = KeyEventResponse {
                token: Some(state.token()?),
                ..Default::default()
            };
            self.request_edit_session(
                state.context.clone(),
                response,
                EditStep::DisconnectComposition,
                owner.clone(),
            )
        })();
        if let Err(error) = result {
            state.disconnect_requested.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(self.lock(&state.composition)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_context_old_connection_generation_and_revision() {
        let token = ContextToken {
            context_id: 3,
            connection_epoch: 7,
            generation: 2,
        };
        let mut route = ResponseRoute::default();
        let mut response = KeyEventResponse {
            token: Some(token.clone()),
            revision: 1,
            ..Default::default()
        };
        assert!(route.accept(&token, &response));
        assert!(!route.accept(&token, &response));
        for bad in [
            ContextToken {
                context_id: 4,
                ..token.clone()
            },
            ContextToken {
                connection_epoch: 6,
                ..token.clone()
            },
            ContextToken {
                generation: 1,
                ..token.clone()
            },
        ] {
            response.token = Some(bad);
            response.revision = 99;
            assert!(!route.accept(&token, &response));
        }
        let fresh = ContextToken {
            connection_epoch: 8,
            ..token
        };
        response.token = Some(fresh.clone());
        response.revision = 1;
        assert!(route.accept(&fresh, &response));
    }
}
