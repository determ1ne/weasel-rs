//! TIP 与进程外组件之间共享的消息类型。

include!(concat!(env!("OUT_DIR"), "/weasel.message.rs"));

/// 进程内使用的统一消息包装。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Envelope {
    /// 本次请求的关联编号，用于将响应与请求对应起来。
    pub request_id: u64,
    /// 已选择的消息类型；为空表示尚未设置有效负载。
    pub payload: Option<envelope::Payload>,
}

/// TIP 编辑流程使用的本地响应适配类型。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyEventResponse {
    /// 服务端用于选择宿主 preedit 路径的内部路由结果，不属于 TIP RPC 管道协议。
    pub external_preedit: bool,
    /// 引擎是否消费了此次按键。
    pub eaten: bool,
    /// 本次按键产生、需要提交到宿主文本的内容。
    pub commit_text: String,
    /// 当前 composition 的显示文本。
    pub composition: String,
    /// 当前 composition 对应的原始编码输入。
    pub raw_input: Option<String>,
    /// 当前候选页中的候选项。
    pub candidates: Vec<Candidate>,
    /// 当前页中选中候选项的索引。
    pub selected_candidate: u32,
    /// composition 光标位置，单位为 UTF-16 code unit。
    pub composition_cursor: u32,
    /// 本次响应是否更新了输入状态。
    pub state_updated: bool,
    /// 引擎当前是否处于 composition 状态。
    pub composing: bool,
    /// 当前候选页在全部候选项中的起始索引。
    pub page_start: u32,
    /// 是否存在上一候选页。
    pub can_page_previous: bool,
    /// 是否存在下一候选页。
    pub can_page_next: bool,
    /// 是否请求宿主打开 emoji 面板。
    pub open_emoji_panel: bool,
    /// 此响应所属的宿主输入上下文生命周期。
    pub token: Option<ContextToken>,
    /// 服务端输入状态的版本，用于拒绝过期的编辑结果。
    pub revision: u64,
    /// 当前 ASCII/中文模式；未知时为空。
    pub ascii_mode: Option<bool>,
    /// 当前安全输入策略；未知时为空。
    pub allow_rime_in_secure_fields: Option<bool>,
}

/// 进程内路由使用的消息负载。
pub mod envelope {
    use super::*;

    /// [`Envelope`] 可承载的进程内请求或响应。
    #[derive(Clone, Debug, PartialEq)]
    pub enum Payload {
        /// 查询当前进程提供的服务身份。
        IdentifyService(IdentifyService),
        /// 返回服务身份信息。
        ServiceIdentity(ServiceIdentity),
        /// 向组件发送用户提示。
        UserNotification(UserNotification),
        /// 请求读取配置项。
        QueryConfig(QueryConfig),
        /// 返回配置值。
        ConfigValue(ConfigValue),
        /// 保活请求。
        Ping(Ping),
        /// 保活响应。
        Pong(Pong),
        /// 传递结构化日志事件。
        LogEvent(LogEvent),
        /// 处理宿主按键。
        KeyEvent(InputKey),
        /// 返回按键处理结果。
        KeyEventResponse(KeyEventResponse),
        /// 请求服务关闭。
        Shutdown(Shutdown),
        /// 返回关闭请求的处理结果。
        ShutdownResponse(ShutdownResponse),
        /// 向候选窗渲染器发布完整界面快照。
        RenderSnapshot(RenderSnapshot),
        /// 接收候选窗渲染器发出的交互事件。
        RendererEvent(RendererEvent),
        /// 更新宿主插入点布局信息。
        LayoutUpdate(LayoutUpdate),
        /// 对输入上下文执行控制操作。
        ContextCommand(ContextCommand),
        /// 返回 RPC 或业务处理失败信息。
        Failure(Failure),
        /// 打开一个 TIP 输入上下文。
        OpenInput(OpenInput),
        /// 返回输入上下文创建结果。
        InputOpened(InputOpened),
    }
}
