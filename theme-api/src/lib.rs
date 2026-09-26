//! 输入法主题的跨实现契约与 UI 状态模型。
//!
//! [`ThemeFactory`] 用于查询主题元数据并在 UI 线程创建后端；工厂要求实现
//! `Send + Sync`，而创建出的 [`ThemeBackend`] 及其原生资源留在创建它的 UI
//! 线程。主题通过 [`CandidateView`] 接收展示快照，通过 [`EventSink`] 报告
//! 用户操作。主题实现应将自己的设置解释为本地配置，并遵守各字段所述的
//! UTF-16、内容身份和驻留展示语义；此 API 不授权主题执行通知 RPC 或记录日志。
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub mod plugin;

/// 与具体 UI 外观无关的主题诊断信息。
///
/// 通知由运行时收集并交给宿主处理；主题实现不得借此字段记录日志或执行通知
/// RPC。`code` 和文本内容由主题定义；宿主若要按代码分支处理，须与该主题约定其
/// 含义和兼容策略，不能假定不同主题采用统一代码表。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ThemeNotice {
    /// 通知级别；宿主可据此选择展示方式。
    pub severity: NoticeSeverity,
    /// 便于机器识别的主题内代码；跨主题含义由各主题自行定义。
    pub code: String,
    /// 面向用户的简短说明。
    pub message: String,
    /// 可选的补充上下文；不得包含要求宿主执行 RPC 的指令。
    pub details: String,
}

/// 主题通知的严重程度。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[allow(dead_code)] // Public theme contract; individual themes may use only warnings.
pub enum NoticeSeverity {
    /// 信息性提示。
    Info,
    /// 可恢复的问题或降级情况。
    Warning,
    /// 影响主题功能的错误。
    Error,
}

/// 一次主题创建的结果，以及创建期间产生的诊断通知。
///
/// 后端创建失败时仍可保留通知，供宿主解释失败或配置回退。该值在主题 API
/// 内部传递；DLL 桥接会将结果编码为 JSON，不跨 ABI 传递 Rust 对象所有权。
pub struct ThemeCreation {
    /// 创建成功时拥有的后端；失败时为主题提供的错误说明。
    pub backend: Result<Box<dyn ThemeBackend>, String>,
    /// 创建期间产生的通知，即使原生后端创建失败也会保留。
    pub notices: Vec<ThemeNotice>,
}

/// 将仅含后端结果的旧式返回值转换为没有通知的创建结果。
///
/// 转换会原样保留成功的后端或失败字符串，并将通知列表置空。
impl From<Result<Box<dyn ThemeBackend>, String>> for ThemeCreation {
    fn from(backend: Result<Box<dyn ThemeBackend>, String>) -> Self {
        Self {
            backend,
            notices: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
/// 描述主题后端支持的展示能力。
pub struct ThemeCapabilities {
    /// 后端能否在宿主应用之外展示输入中的预编辑文本。
    pub preedit: bool,
    /// 后端是否处理处于焦点中的纯模式快照，并可在没有预编辑文本或候选列表时
    /// 保持窗口可见。声明此能力的实现应利用快照的 [`CandidateView::active`]
    /// 状态，在输入上下文不再活动时收起驻留 UI。
    #[serde(default)]
    pub resident: bool,
    /// 后端能否显示由宿主定时、位于插入点附近的中英文模式提示。
    #[serde(default)]
    pub mode_indicator: bool,
}

impl ThemeCapabilities {
    /// 只展示候选项、不提供预编辑展示或驻留 UI 的能力组合。
    pub const CANDIDATES_ONLY: Self = Self {
        preedit: false,
        resident: false,
        mode_indicator: false,
    };
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
/// 中英文模式提示的触发来源。
pub enum ModeIndicatorReason {
    /// 新的可编辑上下文获得焦点。
    Focus,
    /// 用户操作使最终模式发生变化。
    UserSwitch,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
/// 由宿主统一管理生命周期的短暂中英文模式提示。
pub struct ModeIndicator {
    /// 不透明提示标识；相同标识的布局刷新不得重新开始计时。
    pub id: u64,
    /// `false` 表示中文，`true` 表示英文。
    pub ascii_mode: bool,
    /// 触发提示的原因。
    pub reason: ModeIndicatorReason,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
/// 屏幕上用于定位主题 UI 的矩形锚点。
pub struct Anchor {
    /// 左边界坐标。
    pub left: i32,
    /// 上边界坐标。
    pub top: i32,
    /// 右边界坐标。
    pub right: i32,
    /// 下边界坐标。
    pub bottom: i32,
    /// 坐标是否有效；无效时主题不得依赖这些边界定位。
    pub valid: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
/// 一个候选项的主要文本、辅助文本和可用状态。
pub struct CandidateItem {
    /// 候选项的主要展示文本。
    pub primary_text: String,
    /// 次要说明或注释；为空时没有辅助文本。
    pub secondary_text: String,
    /// 候选项是否可被选择或调用。
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
/// 一帧候选 UI 的完整快照。
///
/// 每次渲染均应以收到的快照为准，而不假定它只是对上次状态的增量。主题可以
/// 缓存展示资源，但不能把几何变化误认为内容身份变化；事件通过 [`EventSink`]
/// 返回，并由桥接层绑定到该帧的 [`content_id`](Self::content_id)。
pub struct CandidateView {
    /// 预编辑信息；`None` 表示行内输入，此时展示候选项而不显示输入字段。
    pub preedit: Option<Preedit>,
    /// 不透明的展示内容标识。展示内容或路由变化时会变化，只有几何更新时保持不变；
    /// 它不是 RPC 版本号或会话 ID。回传事件时应使用事件接收帧对应的标识，避免
    /// 将过期 UI 操作应用到后续内容。
    pub content_id: u64,
    /// 当前输入上下文是否仍拥有此展示。驻留主题在输入服务不再活跃于编辑器时
    /// 应隐藏 UI。
    pub active: bool,
    /// 当前 Rime ASCII 模式；`None` 表示引擎无法确定模式，不能据此假定为任一模式。
    pub ascii_mode: Option<bool>,
    /// 独立于候选内容的短暂模式提示；其期限和取消由 Renderer 管理。
    pub mode_indicator: Option<ModeIndicator>,
    /// 当前快照是否要求展示 UI。
    pub visible: bool,
    /// 候选 UI 的定位锚点；缺省或无效锚点表示没有可用的定位几何信息。
    pub anchor: Option<Anchor>,
    /// 当前页候选项，顺序与引擎提供的顺序一致。
    pub items: Vec<CandidateItem>,
    /// 当前选中项在候选集合中的索引。
    pub selected_index: u32,
    /// 当前页第一项在候选集合中的索引。
    pub page_start: u32,
    /// 候选总数；`None` 表示总数未知。
    pub total_item_count: Option<u32>,
    /// 当前是否可以切换到上一页。
    pub can_page_previous: bool,
    /// 当前是否可以切换到下一页。
    pub can_page_next: bool,
}

/// 判断两个快照的展示内容是否相同。
///
/// 比较内容标识、活动状态、模式、预编辑文本、候选项和翻页状态，不比较
/// `visible` 与 `anchor`。因此仅可见性或几何变化不会使结果变为 `false`；调用方
/// 可用此判定决定是否需要更新内容，而仍须单独处理定位和显隐。
pub fn same_content(a: &CandidateView, b: &CandidateView) -> bool {
    a.content_id == b.content_id
        && a.active == b.active
        && a.ascii_mode == b.ascii_mode
        && match (&a.mode_indicator, &b.mode_indicator) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.id == b.id && a.ascii_mode == b.ascii_mode && a.reason == b.reason
            }
            _ => false,
        }
        && a.preedit == b.preedit
        && a.items == b.items
        && a.selected_index == b.selected_index
        && a.page_start == b.page_start
        && a.total_item_count == b.total_item_count
        && a.can_page_previous == b.can_page_previous
        && a.can_page_next == b.can_page_next
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
/// 输入中的预编辑文本及其光标位置。
pub struct Preedit {
    /// 预编辑文本；不能超过 65,536 个 UTF-8 字节，也不能含 NUL 字符。
    pub text: String,
    /// 光标在 UTF-16 码元中的偏移，且必须落在 Unicode 标量边界上。
    pub cursor: u32,
}

impl Preedit {
    /// 检查文本限制和光标边界。
    ///
    /// 成功返回 `Ok(())`；文本含 NUL 或超过字节上限时返回文本错误，光标不在
    /// 有效 UTF-16 标量边界时返回光标错误。偏移量使用 UTF-16 单位，而非 UTF-8
    /// 字节或 Unicode 字符数。
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > 65536 || self.text.contains('\0') {
            return Err("invalid preedit text".into());
        }
        let mut offset = 0;
        for ch in self.text.chars() {
            if offset == self.cursor {
                return Ok(());
            }
            offset += ch.len_utf16() as u32;
        }
        if offset == self.cursor {
            Ok(())
        } else {
            Err("invalid preedit cursor".into())
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
/// 主题 UI 可回传给输入服务的用户操作。
pub enum UiAction {
    /// 调用候选集合中指定索引的候选项。
    ItemInvoked(u32),
    /// 请求切换到上一页候选项。
    NavigatePrevious,
    /// 请求切换到下一页候选项。
    NavigateNext,
    /// 请求关闭当前主题 UI。
    Dismiss,
    /// 请求打开表情面板。
    OpenEmojiPanel,
}

/// 供主题将用户操作提交给宿主的线程安全回调。
///
/// 回调由 [`new`](Self::new) 提供并以引用计数方式共享。桥接实现会将其绑定到
/// 产生该回调的展示帧，而非可变的最新状态；主题应在对应 UI 事件发生时调用，
/// 不应自行改写内容标识或跨帧重绑定事件。
#[derive(Clone)]
pub struct EventSink(Arc<dyn Fn(UiAction) + Send + Sync>);

impl EventSink {
    /// 创建事件接收器。
    ///
    /// 回调必须满足 `Send + Sync + 'static`，因此可由主题持有并从其 UI 回调路径
    /// 调用。主题通常应避免形成阻塞宿主的长时间工作。
    pub fn new(callback: impl Fn(UiAction) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }

    /// 提交一个主题 UI 操作。
    ///
    /// 调用会同步执行接收器回调；具体事件是否被宿主接受由宿主决定，本方法不
    /// 返回确认或错误。
    pub fn send(&self, action: UiAction) {
        (self.0)(action);
    }
}

#[cfg(windows)]
/// 在 UI 线程上运行的主题后端。
///
/// 后端通常拥有窗口、图形对象等线程亲和资源，因此不要求实现 `Send`。运行时在
/// 创建后于同一 UI 线程串行调用其方法，并须在对应 UI apartment 关闭前销毁它。
pub trait ThemeBackend {
    /// 取出自上次收集以来的通知。
    ///
    /// 运行时在每次后端操作后调用，包括操作失败时；默认实现没有通知。主题可在
    /// 此处一次性排空待交付通知，无需自行轮询宿主。
    fn take_notices(&mut self) -> Vec<ThemeNotice> {
        Vec::new()
    }
    /// 按完整快照更新 UI，并通过 `events` 回传用户操作。
    ///
    /// 返回错误表示本次渲染未成功，错误文本会交给宿主；即使失败，运行时仍会
    /// 随后收集通知。实现应处理快照中的隐藏、活动状态及可用几何信息。
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String>;
    /// 隐藏主题 UI。
    ///
    /// 此操作没有结果值；主题应同步完成其隐藏状态更新。桥接层还会清除尚未交付
    /// 的事件队列。
    fn hide(&mut self);
    /// 仅使外观资源失效并按需重建。
    ///
    /// 运行时负责判断当前所有者是否仍允许使用其快照重绘。错误文本会传给宿主，
    /// 操作后产生的通知仍会被收集。
    fn refresh_appearance(&mut self) -> Result<(), String>;
    /// 检查后端健康状态。
    ///
    /// 返回错误表示后端不可继续正常使用；桥接层会将错误传给宿主。默认实现视为
    /// 健康，不执行检查。
    fn check_health(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// 可安全共享的主题工厂与元数据入口。
///
/// 工厂必须实现 `Send + Sync`，元数据查询不得创建原生 UI 资源。运行时只在 UI
/// apartment 调用 [`create`](Self::create)；返回后端不要求 `Send`，并由该线程
/// 串行使用和销毁。设置的键名与语义由各主题自行定义，不能假定不同主题共享
/// 配置格式；宿主负责在创建前合并相应主题的默认值与用户设置。
#[cfg(windows)]
pub trait ThemeFactory: Send + Sync {
    /// 返回主题的静态名称，供宿主识别和展示。
    fn name(&self) -> &'static str;
    /// 返回主题支持的展示能力。
    fn capabilities(&self) -> ThemeCapabilities;
    /// 返回主题级默认设置。
    ///
    /// 宿主会在调用 [`create`](Self::create) 前将安装级或用户设置覆盖到默认值上。
    /// 出错时返回的字符串会成为元数据查询错误；默认实现返回空 JSON 对象。
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::Value::Object(Default::default()))
    }
    /// 根据模式和只读配置创建主题后端。
    ///
    /// 此方法在 UI 线程调用。配置快照在本次调用期间借用，主题若需长期使用其中
    /// 的值，应自行复制所需数据。返回值可同时携带创建通知和后端错误；成功后端
    /// 的原生资源须留在创建线程。错误字符串供宿主显示或诊断。
    fn create(
        &self,
        mode: UiMode,
        settings: &weasel_common::settings::ConfigSnapshot,
    ) -> ThemeCreation;
}

/// 主题后端的运行场景。
///
/// 预览模式用于独立展示皮肤并允许关闭；实时模式由服务端驱动，通常随候选状态
/// 更新。主题可据此调整交互，但展示数据仍以收到的快照为准。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiMode {
    /// 服务端驱动的实际输入展示。
    Live,
    /// 独立、可关闭的主题预览。
    Preview,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preedit_cursor_uses_utf16_boundaries() {
        for (cursor, valid) in [
            (0, true),
            (1, true),
            (2, false),
            (3, true),
            (4, true),
            (5, false),
        ] {
            assert_eq!(
                Preedit {
                    text: "a😀中".into(),
                    cursor
                }
                .validate()
                .is_ok(),
                valid
            );
        }
        assert!(
            Preedit {
                text: String::new(),
                cursor: 0
            }
            .validate()
            .is_ok()
        );
        assert!(
            Preedit {
                text: "\0".into(),
                cursor: 0
            }
            .validate()
            .is_err()
        );
    }
}
