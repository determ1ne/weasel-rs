//! 在宿主与 WASM 主题之间建立受限的 Wasmtime 调用边界。
//!
//! 宿主通过 `Linker<HostState>` 暴露绘制、视图查询、资源测量和交互导入；主题只能
//! 通过这些导入访问宿主能力。宿主则校验 ABI 与导出签名，再以类型化函数调用主题。
//! guest 的 trap、燃料耗尽、无效返回码和宿主导入拒绝都会成为 `Err(String)`，由上层
//! 主题故障路径隔离；事务同时回滚未提交的宿主展示状态，但无法回滚 guest 线性内存。
//!
//! `Engine` 是启用 fuel 的进程级单例；每次创建运行时都从输入字节重新编译模块，
//! 以便预览刷新读取主题新版本，也不加载不可信的本地编译缓存。每个
//! [`WasmRuntime`] 独占一个 `Store`、实例及宿主状态，并绑定创建它的 UI 线程（非 `Send`）。
//! 开始初始化后，丢弃运行时时至多调用一次有 fuel 限额的 `theme_destroy`；即使
//! 清理回调 trap，也由 Wasmtime/宿主持有的资源所有权完成回收。
//!
//! start、初始化、渲染和普通事件分别使用独立燃料预算。Fuel 约束 guest 指令数，宿主
//! 导入另有调用数、文本字节数、原生资源工作量、绘制命令、命中区及线性内存上限；
//! 这些限制共同控制 guest 诱发的计算、分配和昂贵宿主工作。字体与布局资源由
//! resources 模块统一测量，结果作为绘制命令交由 canvas 回放。

use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};
use wasmtime::{
    Caller, Config, Engine, FuncType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc, Val, ValType,
};

use crate::abi::{Action, Capability, EventKind, FrameResult, LogLevel};
use crate::protocol::{
    ABI_VERSION, DrawCommand, ERR_OK, EXPORT_ABI_VERSION, EXPORT_CAPABILITIES, EXPORT_INIT,
    IMPORT_ABORT, IMPORT_ABORT_MODULE, IMPORT_BEGIN_DRAG, IMPORT_FILL_RECT,
    IMPORT_FILL_ROUNDED_RECT, IMPORT_LOG, IMPORT_MODULE, IMPORT_REQUEST_FRAME, IMPORT_SEND_ACTION,
    IMPORT_SET_FIXED_POSITION, IMPORT_SET_PANEL, IMPORT_SET_SIZE, IMPORT_SET_VISIBLE,
    IMPORT_STROKE_RECT, IMPORT_TIME_MS, MAX_STRING_BYTES, MEMORY, MOUSE_DOWN, PanelStyle,
    PlacementStyle,
};

/// 线性内存上限（字节）：主题持有视图副本与布局状态，128 MiB 远超正常需求，
/// 超限的 `memory.grow` 直接 trap（见 `trap_on_grow_failure`）。
pub const MAX_MEMORY_BYTES: usize = 128 * 1024 * 1024;
/// `set_size` 的单边上限（DIP）：约等于任何屏幕的最大物理尺寸。
pub const MAX_DIM_DIP: f32 = 8192.0;

// 每次导出的指令预算（wasmtime fuel）：超出即 trap，trap 被收敛为 Err。
/// start 函数（如 AssemblyScript 静态初始化）在实例化时执行，单列预算。
const INSTANTIATE_FUEL: u64 = 10_000_000;
/// `theme_create` 的指令预算。
const INIT_FUEL: u64 = 1_000_000;
/// 视图渲染回调的指令预算。
const RENDER_FUEL: u64 = 10_000_000;
/// 指针、动画、隐藏和外观变化等事件回调的指令预算。
const EVENT_FUEL: u64 = 1_000_000;

/// 进程级 Wasmtime 引擎（启用 fuel 计量）。
static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

/// 惰性创建并缓存进程级引擎；初始化失败也会缓存，后续调用返回同一错误。
fn engine() -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| {
            let mut config = Config::default();
            config.consume_fuel(true);
            config.wasm_memory64(false);
            Engine::new(&config).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// 一个主题实例对应的宿主状态，随 `Store` 独占并在回调间持续保存。
///
/// WASM 只能通过已注册的导入间接读写其中允许的内容；宿主侧配额计数在每次导出调用
/// 开始时清零。展示数据由 [`WasmRuntime::transact`] 暂存和提交，资源配置、日志等
/// 非展示数据不属于展示快照，因而不会随帧回滚。
pub struct HostState {
    /// ABI 与能力声明查询阶段的门闩；该阶段内调用任一宿主导入都会失败。
    declaration_query: bool,
    /// 宿主资源测量与布局服务；不会把原生资源句柄直接交给 guest。
    pub resources: crate::resources::Resources,
    /// 当前帧候选视图的共享快照，避免查询导入反复复制整份视图。
    pub view: Option<Arc<crate::theme_api::CandidateView>>,
    /// 由元数据默认值与实例选项合并后的主题配置。
    pub options: serde_json::Value,
    /// 宿主提供给主题的数据设置。
    pub settings: serde_json::Value,
    /// 仅由宿主在事件回调期间开启，回调结束后关闭；guest 无需自行配对。
    pub(crate) frame_open: bool,
    /// 已提交及当前事务暂存的图层场景。
    pub(crate) layers: crate::layers::LayerScene,
    /// 当前绘制命令和命中区所归属的图层；`None` 表示主画布。
    pub(crate) selected_layer: Option<u32>,
    pub(crate) layers_edited: bool,
    pub(crate) motion_revision: u64,
    pub(crate) event_revision: u64,
    main_edited: bool,
    main_updated: bool,
    presented_commands: Vec<DrawCommand>,
    pub(crate) draw_depth: usize,
    pending_frame: Option<Vec<DrawCommand>>,
    regions: Vec<HitRegion>,
    pointer_region: i32,
    pointer_layer: i32,
    /// 按下时记录的图层 ID、代次和区域 ID；抬起事件据此防止旧目标误触。
    pub(crate) pressed_target: Option<(u32, u64, i32)>,
    /// 当前导出调用已消耗的宿主导入次数与 UTF-8 文本字节数。
    host_calls: usize,
    text_bytes: usize,
    /// 当前回调是否具备发送用户动作的资格，仅用于按下/抬起事件。
    accept_actions: bool,
    /// 经 `Store::limiter` 生效的 Wasmtime 资源限制器，约束内存、表和实例的增长。
    pub limits: StoreLimits,
    /// 当前帧的绘制命令（host 在 WM_PAINT 回放）。
    pub commands: Vec<DrawCommand>,
    /// 主题最近一次声明的内容尺寸（DIP）。
    pub size: (f32, f32),
    anchor_rect: Option<crate::protocol::Rect>,
    /// 当前事务中的原生面板样式；成功提交后成为宿主展示状态。
    pub panel_style: PanelStyle,
    /// 最近一次成功提交的原生背景材质配置。
    pub backdrop_style: crate::protocol::BackdropStyle,
    /// 展示状态随帧提交；每次视图回调默认可见，常驻主题可显式隐藏。
    pub visible: bool,
    /// 当前事务中的窗口定位方式；成功提交后由宿主应用。
    pub placement: PlacementStyle,
    /// 当前处理的鼠标事件类型；拖动请求仅在指针按下回调中有效。
    mouse_kind: Option<i32>,
    drag_requested: bool,
    /// 主题请求了下一动画帧。
    pub wake_request: crate::animation::WakeRequest,
    /// 主题请求的高层动作 `(action, index)`；事务失败时清空。
    pub actions: Vec<(i32, i32)>,
    /// 用户通知（`report_notice` 导入与 host 侧警告）；普通日志不进入此队列。
    pub notes: Vec<String>,
    /// `measure_text` 调用计数（测试用）。
    pub measure_calls: u32,
    image_calls: u32,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            declaration_query: false,
            resources: Default::default(),
            view: None,
            options: serde_json::json!({}),
            settings: serde_json::json!({}),
            frame_open: false,
            layers: Default::default(),
            selected_layer: None,
            layers_edited: false,
            motion_revision: 0,
            event_revision: 0,
            main_edited: false,
            main_updated: false,
            presented_commands: Vec::new(),
            draw_depth: 0,
            pending_frame: None,
            regions: Vec::new(),
            pointer_region: -1,
            pointer_layer: 0,
            pressed_target: None,
            host_calls: 0,
            text_bytes: 0,
            accept_actions: false,
            limits: default_limits(),
            commands: Vec::new(),
            size: (0.0, 0.0),
            anchor_rect: None,
            panel_style: PanelStyle::default(),
            backdrop_style: Default::default(),
            visible: true,
            placement: PlacementStyle::Anchored,
            mouse_kind: None,
            drag_requested: false,
            wake_request: Default::default(),
            actions: Vec::new(),
            notes: Vec::new(),
            measure_calls: 0,
            image_calls: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HitRegion {
    pub(crate) id: i32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
}
impl HitRegion {
    /// 按圆角矩形几何规则判断点是否命中；坐标相对于所属画布或图层。
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        crate::geometry::hit_content(x - self.x, y - self.y, self.w, self.h, self.radius)
    }
}

/// 回调事务的展示快照；不包含日志、资源配置等非画面状态。
struct Presentation {
    layers: crate::layers::LayerScene,
    anchor_rect: Option<crate::protocol::Rect>,
    size: (f32, f32),
    panel: PanelStyle,
    backdrop: crate::protocol::BackdropStyle,
    visible: bool,
    placement: PlacementStyle,
    view: Option<Arc<crate::theme_api::CandidateView>>,
    regions: Vec<HitRegion>,
}
impl Presentation {
    /// 复制可回滚的展示状态；日志、资源、配额计数和 guest 内存不在快照中。
    fn capture(s: &HostState) -> Self {
        Self {
            layers: s.layers.clone(),
            size: s.size,
            anchor_rect: s.anchor_rect,
            panel: s.panel_style,
            backdrop: s.backdrop_style,
            visible: s.visible,
            placement: s.placement,
            view: s.view.clone(),
            regions: s.regions.clone(),
        }
    }
    /// 恢复失败或保留帧的展示状态，同时保留快照以外的副作用。
    fn restore(self, s: &mut HostState) {
        s.layers = self.layers;
        s.size = self.size;
        s.anchor_rect = self.anchor_rect;
        s.panel_style = self.panel;
        s.backdrop_style = self.backdrop;
        s.visible = self.visible;
        s.placement = self.placement;
        s.view = self.view;
        s.regions = self.regions;
    }
}

impl HostState {
    /// 记录宿主边界调用及其文本字节数；与 Wasmtime fuel 分开限制原生工作入口。
    ///
    /// 声明查询期的导入调用会失败；单次导出最多允许 4096 次调用和累计 1 MiB 文本。
    /// 任一配额超限都会返回错误并中止当前 guest 回调。
    pub(crate) fn charge(&mut self, bytes: usize) -> wasmtime::Result<()> {
        if self.declaration_query {
            return Err(wasmtime::format_err!(
                "host calls are not allowed before theme creation"
            ));
        }
        self.host_calls += 1;
        self.text_bytes = self.text_bytes.saturating_add(bytes);
        if self.host_calls > 4096 || self.text_bytes > 1024 * 1024 {
            return Err(wasmtime::format_err!("theme host-call budget exceeded"));
        }
        Ok(())
    }

    /// 检查是否能修改主展示面，并将操作计入宿主调用预算。
    fn edit_presentation(&mut self) -> wasmtime::Result<()> {
        self.charge(0)?;
        if self.selected_layer.is_some() {
            return Err(wasmtime::format_err!("surface changes are not layer-local"));
        }
        Ok(())
    }

    /// 限制单次回调中的昂贵原生资源工作；图片次数受独立子配额约束。
    pub(crate) fn charge_resource(&mut self, image: bool) -> wasmtime::Result<()> {
        self.measure_calls += 1;
        self.image_calls += u32::from(image);
        if self.measure_calls > 256 || self.image_calls > 4 {
            return Err(wasmtime::format_err!(
                "native resource work budget exceeded"
            ));
        }
        Ok(())
    }
    /// 在事件绘制事务中追加命令，并维护绘制栈、主画布和图层的数量上限。
    /// 非有限坐标、栈不平衡或命令超限都会使当前回调失败。
    pub(crate) fn draw(&mut self, command: DrawCommand) -> wasmtime::Result<()> {
        if !self.frame_open {
            return Err(wasmtime::format_err!(
                "drawing is only allowed in theme_event"
            ));
        }
        if !command.is_finite() {
            return Err(wasmtime::format_err!("non-finite draw coordinates"));
        }
        if self.commands.len() >= 1024 {
            return Err(wasmtime::format_err!("theme draw-command limit exceeded"));
        }
        match &command {
            DrawCommand::PushTransform(_) | DrawCommand::PushClip(_) => {
                if self.draw_depth >= 32 {
                    return Err(wasmtime::format_err!("draw stack limit exceeded"));
                }
                self.draw_depth += 1;
            }
            DrawCommand::PopState => {
                self.draw_depth = self
                    .draw_depth
                    .checked_sub(1)
                    .ok_or_else(|| wasmtime::format_err!("empty draw stack"))?;
            }
            _ => {}
        }
        if let Some(id) = self.selected_layer {
            let layer = self
                .layers
                .layers
                .iter_mut()
                .find(|layer| layer.id == id)
                .ok_or_else(|| wasmtime::format_err!("drawing target layer removed"))?;
            if layer.commands.len() >= 1024 {
                return Err(wasmtime::format_err!("layer command limit"));
            }
            layer.commands.push(command);
            self.layers_edited = true;
        } else {
            self.main_edited = true;
            self.commands.push(command);
        }
        Ok(())
    }
}

/// 为每个 Store 创建独立限制器：线性内存最多 128 MiB，并限制内存、表、表元素及实例数。
/// 增长失败立即 trap，避免 guest 将超限当作可忽略的分配失败。
fn default_limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .memories(1)
        .tables(1)
        .table_elements(1 << 20)
        .instances(4)
        .trap_on_grow_failure(true)
        .build()
}

/// 从当前调用者导出的线性内存复制 UTF-8 字符串。
///
/// 指针按 wasm32 无符号地址解释；负长度、超过协议上限、地址溢出、越界、缺少 `memory`
/// 导出或非法 UTF-8 均返回 `None`。返回字符串归宿主所有，不借用可变的 guest 内存。
pub(crate) fn read_wasm_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if len < 0 || len as usize > MAX_STRING_BYTES {
        return None;
    }
    let memory = caller.get_export(MEMORY)?.into_memory()?;
    let start = ptr as u32 as usize;
    let end = start.checked_add(len as usize)?;
    let bytes = memory.data(&*caller).get(start..end)?;
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

// ── host 导入函数（wasm → host）─────────────────────────────────────

// 导入通过 `Caller` 访问当前实例的 Store，不跨运行时共享 guest 状态。每个入口在执行
// 宿主工作前检查配额；拒绝请求会作为 Wasmtime 错误返回，交由事务层隔离未提交的展示修改。

/// 校验并暂存面板圆角、阴影、偏移和颜色；保留当前事务中的显式边界。
fn set_panel(
    mut caller: Caller<'_, HostState>,
    radius: f32,
    shadow_radius: f32,
    offset_x: f32,
    offset_y: f32,
    color: i32,
) -> wasmtime::Result<()> {
    caller.data_mut().edit_presentation()?;
    if ![radius, shadow_radius, offset_x, offset_y]
        .iter()
        .all(|v| v.is_finite())
        || !(0.0..=4096.0).contains(&radius)
        || !(0.0..=250.0).contains(&shadow_radius)
        || !(-1024.0..=1024.0).contains(&offset_x)
        || !(-1024.0..=1024.0).contains(&offset_y)
    {
        return Err(wasmtime::format_err!("invalid panel style"));
    }
    let bounds = caller.data().panel_style.bounds;
    caller.data_mut().panel_style = PanelStyle {
        bounds,
        corner_radius: radius,
        shadow_radius,
        offset_x,
        offset_y,
        color: color as u32,
    };
    Ok(())
}

/// 校验背景材质开关、模糊半径和总和为 1 的混合权重后更新宿主状态。
fn set_backdrop(
    mut caller: Caller<'_, HostState>,
    enabled: i32,
    tint: i32,
    blur_sigma: f32,
    backdrop_balance: f32,
    afterglow_balance: f32,
    color_balance: f32,
    fallback_color: i32,
) -> wasmtime::Result<()> {
    caller.data_mut().edit_presentation()?;
    let weights = [backdrop_balance, afterglow_balance, color_balance];
    if !matches!(enabled, 0 | 1)
        || !blur_sigma.is_finite()
        || !(0.0..=64.0).contains(&blur_sigma)
        || weights
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        || (weights.iter().sum::<f32>() - 1.0).abs() > 0.001
    {
        return Err(wasmtime::format_err!("invalid backdrop style"));
    }
    caller.data_mut().backdrop_style = crate::protocol::BackdropStyle {
        enabled: enabled != 0,
        tint: tint as u32 | 0xff000000,
        blur_sigma,
        backdrop_balance,
        afterglow_balance,
        color_balance,
        fallback_color: fallback_color as u32 | 0xff000000,
    };
    Ok(())
}

/// 将圆角矩形填充请求转换为受限的绘制命令。
fn fill_rounded_rect(
    mut caller: Caller<HostState>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: u32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    caller.data_mut().draw(DrawCommand::FillRoundedRect {
        x,
        y,
        w,
        h,
        radius,
        color,
    })
}

/// 将矩形填充请求转换为受限的绘制命令。
fn fill_rect(
    mut caller: Caller<'_, HostState>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    caller
        .data_mut()
        .draw(DrawCommand::FillRect { x, y, w, h, color })
}

/// 将矩形描边请求转换为受限的绘制命令。
fn stroke_rect(
    mut caller: Caller<'_, HostState>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    width: f32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    caller.data_mut().draw(DrawCommand::StrokeRect {
        x,
        y,
        w,
        h,
        color,
        width,
    })
}

/// 设置 DIP 内容尺寸并清除依赖旧尺寸的锚点和面板边界。
fn set_size(mut caller: Caller<'_, HostState>, w: f32, h: f32) -> wasmtime::Result<()> {
    caller.data_mut().edit_presentation()?;
    let st = caller.data_mut();
    if ![w, h]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0 && *v <= MAX_DIM_DIP)
    {
        return Err(wasmtime::format_err!("invalid content size"));
    }
    st.size = (w, h);
    st.panel_style.bounds = None;
    st.anchor_rect = None;
    Ok(())
}

/// 设置本次展示事务提交后的可见状态；参数必须为 0 或 1。
fn set_visible(mut caller: Caller<'_, HostState>, visible: i32) -> wasmtime::Result<()> {
    caller.data_mut().edit_presentation()?;
    if !matches!(visible, 0 | 1) {
        return Err(wasmtime::format_err!("set_visible expects 0 or 1"));
    }
    caller.data_mut().visible = visible != 0;
    Ok(())
}

/// 将窗口定位改为受限坐标范围内的固定 DIP 位置。
fn set_fixed_position(mut caller: Caller<'_, HostState>, x: f32, y: f32) -> wasmtime::Result<()> {
    caller.data_mut().edit_presentation()?;
    if !x.is_finite()
        || !y.is_finite()
        || !(-8192.0..=8192.0).contains(&x)
        || !(-8192.0..=8192.0).contains(&y)
    {
        return Err(wasmtime::format_err!("invalid fixed window position"));
    }
    caller.data_mut().placement = PlacementStyle::Fixed { x, y };
    Ok(())
}

/// 仅在处理指针按下事件时登记拖动请求；其他时机记诊断并忽略。
fn begin_drag(mut caller: Caller<'_, HostState>) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    if caller.data().mouse_kind != Some(MOUSE_DOWN) {
        caller
            .data_mut()
            .notes
            .push("begin_drag ignored outside pointer-down".into());
        return Ok(());
    }
    caller.data_mut().drag_requested = true;
    Ok(())
}

/// 在指针按下/抬起回调中暂存有效动作；回调失败会丢弃，单次最多 16 项。
fn send_action(mut caller: Caller<'_, HostState>, action: i32, index: i32) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    let st = caller.data_mut();
    if !st.accept_actions {
        return Err(wasmtime::format_err!(
            "send_action requires a pointer down/up event"
        ));
    }
    if st.actions.len() >= 16 {
        return Err(wasmtime::format_err!("too many theme actions"));
    }
    if Action::try_from(action).is_err() || index < 0 {
        st.notes.push(format!(
            "send_action ignored: action={action} index={index}"
        ));
        return Ok(());
    }
    st.actions.push((action, index));
    Ok(())
}

/// 请求约 16 毫秒后的下一帧，并受当前导出调用的宿主预算约束。
fn request_frame(mut caller: Caller<'_, HostState>) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    caller.data_mut().wake_request.request(now_ms() + 16.0);
    Ok(())
}

/// 将有效 UTF-8 通知复制到有界诊断队列；非法内存切片不追加内容。
fn report_notice(caller: Caller<'_, HostState>, msg: i32, len: i32) -> wasmtime::Result<()> {
    let mut caller = caller;
    caller.data_mut().charge(len.max(0) as usize)?;
    if !(0..=4096).contains(&len) || caller.data().notes.len() >= 64 {
        return Err(wasmtime::format_err!("theme log budget exceeded"));
    }
    if let Some(text) = read_wasm_string(&mut caller, msg, len) {
        caller.data_mut().notes.push(text);
    }
    Ok(())
}

/// 将 AssemblyScript 的 abort 转为 trap，仅保留源码行列号。
///
/// 其消息与文件参数是托管 UTF-16 对象指针而非字节长度；这里不解析运行时私有对象布局，
/// 以免依赖 guest 的内存表示。
fn abort(
    _caller: Caller<'_, HostState>,
    _message: i32,
    _file: i32,
    line: i32,
    column: i32,
) -> wasmtime::Result<()> {
    Err(wasmtime::format_err!(
        "AssemblyScript aborted at line {line}, column {column}"
    ))
}

/// 返回进程内单调递增的毫秒时钟；时间原点未指定，仅适合计算间隔和动画截止时间。
pub(crate) fn now_ms() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// 注册主题 ABI 的完整宿主导入面；签名注册失败时停止构造并返回上下文错误。
///
/// 导入处理的数据仍归属于调用方 Store；字符串从线性内存做有界复制，绘制、命中区、
/// 动作和资源操作都执行各自校验或配额检查。
fn register_imports(linker: &mut Linker<HostState>) -> Result<(), String> {
    let register = |result: wasmtime::Result<&mut Linker<HostState>>| -> Result<(), String> {
        result
            .map(|_| ())
            .map_err(|e| format!("failed to register imports: {e}"))
    };
    register(linker.func_wrap(
        IMPORT_MODULE,
        "hit_region",
        |mut c: Caller<'_, HostState>,
         id: i32,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         radius: f32|
         -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            let s = c.data_mut();
            if !s.frame_open
                || id <= 0
                || s.regions.len()
                    + s.layers
                        .layers
                        .iter()
                        .map(|l| l.regions.len())
                        .sum::<usize>()
                    >= 256
                || ![x, y, w, h, radius].iter().all(|v| v.is_finite())
                || w <= 0.0
                || h <= 0.0
                || radius < 0.0
            {
                return Err(wasmtime::format_err!("invalid hit region"));
            }
            let regions = match s.selected_layer {
                Some(id) => {
                    &mut s
                        .layers
                        .layers
                        .iter_mut()
                        .find(|l| l.id == id)
                        .unwrap()
                        .regions
                }
                None => &mut s.regions,
            };
            if regions.iter().any(|r| r.id == id) {
                return Err(wasmtime::format_err!("duplicate region id"));
            }
            regions.push(HitRegion {
                id,
                x,
                y,
                w,
                h,
                radius,
            });
            Ok(())
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "pointer_region",
        |mut c: Caller<'_, HostState>| -> wasmtime::Result<i32> {
            c.data_mut().charge(0)?;
            Ok(c.data().pointer_region)
        },
    ))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_PANEL, set_panel))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "pointer_layer",
        |mut c: Caller<'_, HostState>| -> wasmtime::Result<i32> {
            c.data_mut().charge(0)?;
            Ok(c.data().pointer_layer)
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        crate::protocol::IMPORT_SET_BACKDROP,
        set_backdrop,
    ))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_FILL_RECT, fill_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_FILL_ROUNDED_RECT, fill_rounded_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_STROKE_RECT, stroke_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_SIZE, set_size))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_VISIBLE, set_visible))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_FIXED_POSITION, set_fixed_position))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_BEGIN_DRAG, begin_drag))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SEND_ACTION, send_action))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_REQUEST_FRAME, request_frame))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "request_wakeup",
        |mut c: Caller<'_, HostState>, deadline: f64| -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            if !deadline.is_finite() || deadline < 0.0 || deadline > now_ms() + 86_400_000.0 {
                return Err(wasmtime::format_err!("invalid wakeup deadline"));
            }
            c.data_mut().wake_request.request(deadline);
            Ok(())
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "cancel_wakeup",
        |mut c: Caller<'_, HostState>| -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            c.data_mut().wake_request.cancel();
            Ok(())
        },
    ))?;
    // time_ms 无参数：func_wrap 不支持零参闭包，用 func_new 显式给出类型。
    register(linker.func_new(
        IMPORT_MODULE,
        IMPORT_TIME_MS,
        FuncType::new(linker.engine(), [], [ValType::F64]),
        |mut caller, _params, results| {
            caller.data_mut().charge(0)?;
            // Val::F64 携带 IEEE 754 位模式（u64）。
            results[0] = Val::F64(now_ms().to_bits());
            Ok(())
        },
    ))?;
    register(linker.func_wrap(IMPORT_MODULE, "report_notice", report_notice))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_LOG,
        |mut c: Caller<'_, HostState>, level: i32, ptr: i32, len: i32| -> wasmtime::Result<()> {
            c.data_mut().charge(len.max(0) as usize)?;
            let level = LogLevel::try_from(level)
                .map_err(|_| wasmtime::format_err!("invalid log level"))?;
            if !(0..=4096).contains(&len) {
                return Err(wasmtime::format_err!("invalid log arguments"));
            }
            let message = read_wasm_string(&mut c, ptr, len)
                .ok_or_else(|| wasmtime::format_err!("invalid log UTF-8"))?;
            use weasel_common::logging::{ComponentLogger, Level};
            static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
            let level = match level {
                LogLevel::Trace => Level::TRACE,
                LogLevel::Debug => Level::DEBUG,
                LogLevel::Info => Level::INFO,
                LogLevel::Warn => Level::WARN,
                LogLevel::Error => Level::ERROR,
            };
            LOGGER.get_or_init(ComponentLogger::stderr).record(
                level,
                "wasm-theme",
                format_args!("{message}"),
            );
            Ok(())
        },
    ))?;
    // AssemblyScript 运行时导入 env.abort（断言/panic 入口）；缺失则实例化失败。
    register(linker.func_wrap(IMPORT_ABORT_MODULE, IMPORT_ABORT, abort))?;
    crate::data::register(linker).map_err(err_string)?;
    crate::resources::register(linker).map_err(err_string)?;
    crate::layer_api::register(linker).map_err(err_string)?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "push_transform",
        |mut c: Caller<'_, HostState>,
         a: f32,
         b: f32,
         d: f32,
         e: f32,
         x: f32,
         y: f32|
         -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            c.data_mut()
                .draw(DrawCommand::PushTransform([a, b, d, e, x, y]))
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "push_clip",
        |mut c: Caller<'_, HostState>, x: f32, y: f32, w: f32, h: f32| -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            c.data_mut()
                .draw(DrawCommand::PushClip(crate::protocol::Rect { x, y, w, h }))
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "pop_draw_state",
        |mut c: Caller<'_, HostState>| -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            c.data_mut().draw(DrawCommand::PopState)
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "frame_geometry",
        |mut c: Caller<'_, HostState>,
         w: f32,
         h: f32,
         x: f32,
         y: f32,
         aw: f32,
         ah: f32|
         -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            let rect = crate::protocol::Rect { x, y, w: aw, h: ah };
            if !c.data().frame_open
                || c.data().selected_layer.is_some()
                || ![w, h]
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= MAX_DIM_DIP)
                || !rect.within((w, h))
            {
                return Err(wasmtime::format_err!("invalid content/anchor geometry"));
            }
            c.data_mut().size = (w, h);
            c.data_mut().anchor_rect = Some(rect);
            Ok(())
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "panel_bounds",
        |mut c: Caller<'_, HostState>, x: f32, y: f32, w: f32, h: f32| -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            if !c.data().frame_open
                || c.data().selected_layer.is_some()
                || ![x, y, w, h].iter().all(|v| v.is_finite())
                || x < 0.0
                || y < 0.0
                || w <= 0.0
                || h <= 0.0
            {
                return Err(wasmtime::format_err!("invalid panel bounds"));
            }
            c.data_mut().panel_style.bounds = Some(crate::protocol::Rect { x, y, w, h });
            Ok(())
        },
    ))?;
    Ok(())
}

// ── 运行时 ───────────────────────────────────────────────────────────

/// 一个已实例化的主题模块及其宿主状态。
///
/// 每个运行时独占 Store 和实例，且绑定到创建它的 UI 线程（非 `Send`）。导出句柄不得
/// 脱离 Store 使用；开始初始化后，丢弃时至多调用一次可选 `theme_destroy`，该清理
/// 回调也使用有限燃料，失败不会阻止宿主释放 Wasmtime 资源。
pub struct WasmRuntime {
    store: Store<HostState>,
    /// 与 Store 一同持有实例；显式保留实例生命周期直到运行时销毁。
    #[allow(dead_code)]
    instance: wasmtime::Instance,
    init_fn: TypedFunc<(i32, i32), i32>,
    event_fn: TypedFunc<(i32, i32, f32, f32, f64), i32>,
    resident: bool,
    preedit: bool,
    destroy_fn: Option<TypedFunc<(), ()>>,
    initialized: bool,
}

impl std::fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmRuntime").finish()
    }
}

/// 将 ABI 状态码转换为统一错误；只有 `ERR_OK` 表示成功。
fn check_code(code: i32, what: &str) -> Result<(), String> {
    if code == ERR_OK {
        Ok(())
    } else {
        Err(format!("theme {what} failed with error code {code}"))
    }
}

/// 把 Wasmtime 错误链转为可读字符串，保留 backtrace 与 fuel 耗尽、越界等根因。
fn err_string(e: wasmtime::Error) -> String {
    let mut out = String::new();
    for (i, err) in e.chain().enumerate() {
        if i > 0 {
            out.push_str("\ncaused by: ");
        }
        out.push_str(&err.to_string());
    }
    out
}

impl WasmRuntime {
    /// 以默认宿主状态编译并实例化主题模块。
    ///
    /// 模块从传入字节重新编译；实例化期间的 start 函数有独立燃料预算。构造会校验元数据、
    /// 导入、必需导出、ABI 版本和能力位，任何编译、链接、trap 或协议错误均返回字符串，
    /// 不会产生可用的半初始化运行时。
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        Self::with_state(bytes, HostState::default())
    }

    /// 以实例私有宿主状态编译并实例化主题。
    ///
    /// 元数据默认值与 `state.options` 深度合并，选项必须为对象，合并结果最多 1 MiB。
    /// ABI/能力查询期间禁止宿主导入；通过查询后才开放完整导入。Store 持有传入的资源、
    /// 配置和限额，运行时之间互不共享这些状态。
    pub fn with_state(bytes: &[u8], mut state: HostState) -> Result<Self, String> {
        let metadata = weasel_common::wasm_metadata::read(bytes)?;
        let mut defaults = metadata
            .map(|mut m| m["defaults"].take())
            .unwrap_or_else(|| serde_json::json!({}));
        if !state.options.is_object() {
            return Err("theme options must be an object".into());
        }
        weasel_common::settings::merge(&mut defaults, state.options);
        if defaults.to_string().len() > 1024 * 1024 {
            return Err("merged theme config exceeds 1 MiB".into());
        }
        state.options = defaults;
        state.declaration_query = true;
        let engine = engine()?;
        let mut store = Store::new(engine, state);
        // 资源限额：memory.grow 等超限时失败（trap_on_grow_failure 时直接 trap）。
        store.limiter(|s| &mut s.limits);
        // start 函数（如 AssemblyScript 静态初始化）在实例化时执行：
        // consume_fuel 下 store 初始 fuel 为 0，不设预算则立即耗尽 trap。
        store.set_fuel(INSTANTIATE_FUEL).map_err(err_string)?;
        let module = Module::new(engine, bytes).map_err(err_string)?;
        let mut linker = Linker::<HostState>::new(engine);
        register_imports(&mut linker)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(err_string)?;
        let _memory = instance
            .get_memory(&mut store, MEMORY)
            .ok_or_else(|| format!("theme module lacks `{MEMORY}` memory export"))?;
        let export_error = |e: wasmtime::Error| -> String {
            format!("theme module export missing or wrong type: {e}")
        };
        let init_fn = instance
            .get_typed_func(&mut store, EXPORT_INIT)
            .map_err(&export_error)?;
        let version = instance
            .get_typed_func::<(), i32>(&mut store, EXPORT_ABI_VERSION)
            .map_err(&export_error)?
            .call(&mut store, ())
            .map_err(err_string)?;
        if version != ABI_VERSION {
            return Err(format!(
                "unsupported WASM theme ABI {version}, expected {ABI_VERSION}"
            ));
        }
        let capabilities = instance
            .get_typed_func::<(), i32>(&mut store, EXPORT_CAPABILITIES)
            .map_err(&export_error)?
            .call(&mut store, ())
            .map_err(err_string)? as u32;
        if capabilities & !((Capability::Preedit as u32) | (Capability::Resident as u32)) != 0 {
            return Err("unsupported theme capability bits".into());
        }
        store.data_mut().declaration_query = false;
        let event_fn = instance
            .get_typed_func(&mut store, "theme_event")
            .map_err(&export_error)?;
        let destroy_fn = if instance.get_export(&mut store, "theme_destroy").is_some() {
            Some(
                instance
                    .get_typed_func(&mut store, "theme_destroy")
                    .map_err(&export_error)?,
            )
        } else {
            None
        };
        Ok(Self {
            destroy_fn,
            initialized: false,
            store,
            instance,
            init_fn,
            event_fn,
            resident: capabilities & Capability::Resident as u32 != 0,
            preedit: capabilities & Capability::Preedit as u32 != 0,
        })
    }

    /// 为下一次导出调用装载燃料并重置逐回调配额与暂存输出。
    /// 燃料耗尽在 Wasmtime 中成为 trap，调用方再统一映射为 `Err(String)`。
    fn set_fuel(&mut self, budget: u64) -> Result<(), String> {
        let state = self.store.data_mut();
        state.event_revision = state.motion_revision;
        state.host_calls = 0;
        state.text_bytes = 0;
        state.measure_calls = 0;
        state.image_calls = 0;
        state.commands.clear();
        state.selected_layer = None;
        state.layers_edited = false;
        state.main_edited = false;
        state.main_updated = false;
        state.frame_open = false;
        state.pending_frame = None;
        state.actions.clear();
        state.wake_request = Default::default();
        state.accept_actions = false;
        self.store.set_fuel(budget).map_err(err_string)
    }

    /// 检查宿主要求的外部 preedit 能力是否由主题声明支持。
    /// 此检查不调用 guest，也不更改实例状态；主题配置已在构造时合并。
    pub fn configure(&mut self, preedit: bool) -> Result<(), String> {
        if preedit && !self.preedit {
            return Err("theme does not support external preedit".into());
        }
        Ok(())
    }

    /// 用宿主快照包裹一次 guest 回调，并按 `FrameResult` 决定提交或回滚展示状态。
    ///
    /// `Present` 在验证绘制栈、命中区、图层及几何配额后提交完整展示帧；`Keep` 丢弃展示
    /// 改动但保留成功回调产生的合法非展示副作用（如唤醒请求）。初始化的 `Keep` 保留
    /// 初始化设置。回调或验证失败时恢复宿主展示快照并清空动作、拖动和唤醒请求；guest
    /// 线性内存及内部变量无法回滚，因此由上层故障处理决定是否继续使用实例。
    fn transact(
        &mut self,
        budget: u64,
        initialize: bool,
        call: impl FnOnce(&mut Self) -> Result<FrameResult, String>,
    ) -> Result<(), String> {
        let before = Presentation::capture(self.store.data());
        let old_view = before.view.clone();
        let old_regions = before.regions.clone();
        self.set_fuel(budget)?;
        {
            let state = self.store.data_mut();
            state.frame_open = !initialize;
            state.draw_depth = 0;
            if !initialize {
                state.regions.clear();
            }
        }
        let result = call(self).and_then(|outcome| {
            let state = self.store.data();
            let main_regions = if state.main_edited || !state.layers_edited {
                state.regions.len()
            } else {
                old_regions.len()
            };
            if outcome == FrameResult::Present
                && main_regions
                    + state
                        .layers
                        .layers
                        .iter()
                        .map(|l| l.regions.len())
                        .sum::<usize>()
                    > 256
            {
                return Err("hit region quota exceeded".into());
            }
            if outcome == FrameResult::Present && !state.layers.validate(96) {
                return Err("invalid layer scene or quota exceeded".into());
            }
            if state.draw_depth != 0 {
                return Err("unbalanced draw stack".into());
            }
            if outcome == FrameResult::Present
                && (state.anchor_rect.is_some_and(|r| !r.within(state.size))
                    || state
                        .panel_style
                        .bounds
                        .is_some_and(|r| !r.within(state.size)))
            {
                return Err("panel/anchor bounds outside content".into());
            }
            Ok(outcome)
        });
        let state = self.store.data_mut();
        state.frame_open = false;
        if result.is_err() || (!initialize && result != Ok(FrameResult::Present)) {
            before.restore(state);
        }
        if result == Ok(FrameResult::Present) {
            state.main_updated = state.main_edited || !state.layers_edited;
            if !state.main_updated {
                state.view = old_view;
                state.regions = old_regions;
            }
            if state.main_updated {
                state.presented_commands = std::mem::take(&mut state.commands);
            }
            state.pending_frame = Some(state.presented_commands.clone());
        } else {
            state.commands.clear();
        }
        if result.is_err() {
            state.actions.clear();
            state.drag_requested = false;
            state.wake_request = Default::default();
        }
        state.mouse_kind = None;
        state.accept_actions = false;
        result.map(|_| ())
    }

    /// 调用主题创建回调并标记实例进入可清理生命周期。
    ///
    /// 初始化使用专属燃料预算。标记在调用前设置，因此即使创建回调失败，丢弃时仍会尝试
    /// 有限预算的 `theme_destroy`（若主题提供该导出）。
    pub fn init(&mut self, mode: i32, dark: bool) -> Result<(), String> {
        self.initialized = true;
        self.transact(INIT_FUEL, true, |rt| {
            let code = rt
                .init_fn
                .call(&mut rt.store, (mode, dark as i32))
                .map_err(err_string)?;
            check_code(code, "create")?;
            Ok(FrameResult::Keep)
        })
    }

    /// 用候选视图快照调用主题渲染事件。
    ///
    /// 新视图通过 `Arc` 保存在宿主状态；每次成功渲染默认恢复可见。渲染有较大的独立
    /// fuel 预算，只有返回 `Present` 并通过事务验证后才发布绘制结果和命中区域。
    pub fn render(&mut self, view: &crate::theme_api::CandidateView) -> Result<(), String> {
        // 新快照不继承旧候选的按下状态，即使图层和区域ID被复用。
        self.store.data_mut().pressed_target = None;
        let view = Arc::new(view.clone());
        self.transact(RENDER_FUEL, false, |rt| {
            rt.store.data_mut().view = Some(view);
            rt.store.data_mut().visible = true;
            let code = rt
                .event_fn
                .call(
                    &mut rt.store,
                    (EventKind::View as i32, 0, 0.0, 0.0, now_ms()),
                )
                .map_err(err_string)?;
            FrameResult::try_from(code)
                .map_err(|code| format!("theme render failed with code {code}"))
        })
    }

    /// 依据当前已提交图层和命中区域分发指针事件。
    ///
    /// 按下目标带有图层代次标识；抬起时目标必须仍相同才会向 guest 传递命中，否则按未命中
    /// 处理，避免视图刷新后复用的区域 ID 误触。动作只允许由按下/抬起事件产生。
    pub fn mouse(&mut self, kind: i32, x: f32, y: f32) -> Result<(), String> {
        let target = self
            .store
            .data()
            .layers
            .hit(x, y, std::time::Instant::now())
            .unwrap_or((0, 0, self.hit_test(x, y)));
        let mut region = target.2;
        let state = self.store.data_mut();
        if kind == crate::protocol::MOUSE_DOWN {
            state.pressed_target = Some(target);
        }
        if kind == crate::protocol::MOUSE_UP {
            if state.pressed_target != Some(target) {
                region = -1;
            }
            state.pressed_target = None;
        }
        if matches!(
            kind,
            crate::protocol::MOUSE_LEAVE | crate::protocol::MOUSE_CANCEL
        ) {
            state.pressed_target = None;
        }
        self.transact(EVENT_FUEL, false, |rt| {
            let state = rt.store.data_mut();
            state.pointer_layer = if region > 0
                && !matches!(
                    kind,
                    crate::protocol::MOUSE_LEAVE | crate::protocol::MOUSE_CANCEL
                ) {
                target.0 as i32
            } else {
                0
            };
            state.pointer_region = if matches!(
                kind,
                crate::protocol::MOUSE_LEAVE | crate::protocol::MOUSE_CANCEL
            ) {
                -1
            } else {
                region
            };
            state.accept_actions = matches!(
                kind,
                crate::protocol::MOUSE_DOWN | crate::protocol::MOUSE_UP
            );
            state.mouse_kind = Some(kind);
            state.drag_requested = false;
            {
                let code = rt
                    .event_fn
                    .call(
                        &mut rt.store,
                        (EventKind::Pointer as i32, kind, x, y, now_ms()),
                    )
                    .map_err(err_string)?;
                FrameResult::try_from(code)
                    .map_err(|code| format!("theme pointer failed with code {code}"))
            }
        })
    }

    /// 以调用方提供的单调时钟时间触发动画事件，并按普通事件预算执行。
    pub fn frame(&mut self, now_ms: f64) -> Result<(), String> {
        self.transact(EVENT_FUEL, false, |rt| {
            let code = rt
                .event_fn
                .call(
                    &mut rt.store,
                    (EventKind::Animation as i32, 0, 0.0, 0.0, now_ms),
                )
                .map_err(err_string)?;
            FrameResult::try_from(code)
                .map_err(|code| format!("theme animation failed with code {code}"))
        })
    }

    /// 通知主题隐藏，然后无条件清除宿主视图、图层、已呈现命令和待处理交互请求。
    /// 即使 guest 隐藏回调失败，宿主仍完成清理并返回该错误。
    pub fn hide(&mut self) -> Result<(), String> {
        self.store.data_mut().pressed_target = None;
        let result = self.transact(EVENT_FUEL, false, |rt| {
            let code = rt
                .event_fn
                .call(
                    &mut rt.store,
                    (EventKind::Hide as i32, 0, 0.0, 0.0, now_ms()),
                )
                .map_err(err_string)?;
            FrameResult::try_from(code)
                .map_err(|code| format!("theme hide failed with code {code}"))
        });
        let state = self.store.data_mut();
        state.view = None;
        state.layers = Default::default();
        state.presented_commands.clear();
        state.visible = false;
        state.actions.clear();
        state.drag_requested = false;
        state.wake_request = Default::default();
        result
    }

    /// 通知主题外观模式变化；展示改动仍受常规事件事务与预算约束。
    pub fn refresh(&mut self, dark: bool) -> Result<(), String> {
        self.transact(EVENT_FUEL, false, |rt| {
            let code = rt
                .event_fn
                .call(
                    &mut rt.store,
                    (
                        EventKind::Appearance as i32,
                        dark as i32,
                        0.0,
                        0.0,
                        now_ms(),
                    ),
                )
                .map_err(err_string)?;
            FrameResult::try_from(code)
                .map_err(|code| format!("theme appearance failed with code {code}"))
        })
    }

    /// 取走最近一次已提交的帧命令；没有新帧时返回 `None`，空绘制帧仍为 `Some(vec![])`。
    pub fn take_frame(&mut self) -> Option<Vec<DrawCommand>> {
        self.store.data_mut().pending_frame.take()
    }

    #[cfg(test)]
    /// 测试辅助接口：取出当前帧命令；没有待取帧时返回空向量。
    pub fn take_commands(&mut self) -> Vec<DrawCommand> {
        self.take_frame().unwrap_or_default()
    }

    /// 返回最近一次成功提交的面板样式副本。
    pub fn panel_style(&self) -> PanelStyle {
        self.store.data().panel_style
    }
    /// 返回最近一次成功提交的背景材质样式副本。
    pub fn backdrop_style(&self) -> crate::protocol::BackdropStyle {
        self.store.data().backdrop_style
    }

    /// 面板圆角（DIP）。
    pub fn corner_radius(&self) -> f32 {
        self.panel_style().corner_radius
    }

    /// 返回主题声明的内容尺寸，单位为 DIP。
    pub fn size(&self) -> (f32, f32) {
        self.store.data().size
    }

    /// 查询已提交画面的命中结果；后声明区域优先，`-1` 表示未命中，`0` 表示默认面板。
    /// 动画图层使用当前原生时间采样，并按裁剪和交互标志过滤区域。
    pub fn hit_test(&self, x: f32, y: f32) -> i32 {
        let s = self.store.data();
        if let Some((_, _, region)) = s.layers.hit(x, y, std::time::Instant::now()) {
            return region;
        }
        if !s.regions.is_empty() {
            return s
                .regions
                .iter()
                .rev()
                .find(|r| r.contains(x, y))
                .map_or(-1, |r| r.id);
        }
        if s.layers
            .layers
            .iter()
            .any(|l| l.interactive && !l.regions.is_empty())
        {
            return -1;
        }
        let rect = s.panel_style.bounds.unwrap_or(crate::protocol::Rect {
            x: 0.0,
            y: 0.0,
            w: s.size.0,
            h: s.size.1,
        });
        if crate::geometry::hit_content(
            x - rect.x,
            y - rect.y,
            rect.w,
            rect.h,
            s.panel_style.corner_radius,
        ) {
            0
        } else {
            -1
        }
    }

    /// 返回主题显式设置的锚点矩形；未设置时以整个内容区域作为锚点。
    pub fn anchor_rect(&self) -> crate::protocol::Rect {
        self.store
            .data()
            .anchor_rect
            .unwrap_or(crate::protocol::Rect {
                x: 0.0,
                y: 0.0,
                w: self.size().0,
                h: self.size().1,
            })
    }
    /// 主题是否声明可常驻运行。
    pub fn resident(&self) -> bool {
        self.resident
    }

    /// 当前已提交展示状态的可见标志。
    pub fn visible(&self) -> bool {
        self.store.data().visible
    }

    /// 只读访问当前已提交的图层场景。
    pub fn layers(&self) -> &crate::layers::LayerScene {
        &self.store.data().layers
    }
    /// 最近一次已提交帧是否更新了主画布；装饰图层帧可为假。
    pub fn main_updated(&self) -> bool {
        self.store.data().main_updated
    }

    /// 返回当前已提交的窗口放置方式。
    pub fn placement(&self) -> PlacementStyle {
        self.store.data().placement
    }

    /// 取走并清除待处理的拖动请求。
    pub fn take_drag_request(&mut self) -> bool {
        std::mem::take(&mut self.store.data_mut().drag_requested)
    }

    /// 取走并清除主题请求的动画唤醒信息。
    pub fn take_frame_request(&mut self) -> crate::animation::WakeRequest {
        std::mem::take(&mut self.store.data_mut().wake_request)
    }

    /// 取走并清除本次回调产生的高层动作 `(action, index)`。
    pub fn take_actions(&mut self) -> Vec<(i32, i32)> {
        std::mem::take(&mut self.store.data_mut().actions)
    }

    /// 取走并清除主题通知和宿主侧警告；普通日志走独立日志通道。
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().notes)
    }

    /// 返回最近一次导出回调的资源测量调用数，与 WASM 指令燃料分别计量。
    pub fn measure_calls(&self) -> u32 {
        self.store.data().measure_calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_api::{CandidateItem, CandidateView};

    fn module(body: &str, version: i32) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
          (memory (export "memory") 1 1)
          (func (export "theme_abi_version") (result i32) i32.const {version})
          (func (export "theme_capabilities") (result i32) i32.const 0)
          (func (export "theme_create") (param i32 i32) (result i32) i32.const 0)
          (func (export "theme_event") (param i32 i32 f32 f32 f64) (result i32) {body})
          (func (export "mouse") (param i32 f32 f32))
          (func (export "frame") (param f64))
          (func (export "hide"))
          (func (export "refresh") (param i32)))"#
        ))
        .unwrap()
    }

    #[test]
    fn generated_import_signatures_match_host() {
        mod abi {
            include!("abi_generated.rs");
        }
        let mut store = Store::new(engine().unwrap(), HostState::default());
        let mut linker = Linker::new(engine().unwrap());
        register_imports(&mut linker).unwrap();
        for &(name, params, results) in abi::IMPORTS {
            let function = linker
                .get(&mut store, IMPORT_MODULE, name)
                .unwrap()
                .into_func()
                .unwrap();
            let ty = function.ty(&store);
            assert_eq!(
                ty.params().map(|v| v.to_string()).collect::<Vec<_>>(),
                params,
                "{name} params"
            );
            assert_eq!(
                ty.results().map(|v| v.to_string()).collect::<Vec<_>>(),
                results,
                "{name} results"
            );
        }
    }

    #[test]
    fn interactive_layers_follow_motion_clip_and_generation() {
        use crate::layers::{LayerMotion, LayerOperation, LayerProperty, LayerScene, LayerState};
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let offset = LayerMotion {
            snap_from: false,
            property: LayerProperty::OffsetX,
            easing: crate::abi::Easing::Linear,
            revision: 1,
            operation: LayerOperation::Animate,
            from: 0.0,
            to: 20.0,
            start,
            deadline: start + Duration::from_secs(1),
        };
        let mut scale = offset;
        scale.property = LayerProperty::ScaleX;
        scale.operation = LayerOperation::Set;
        scale.to = 2.0;
        let mut scene = LayerScene {
            layers: vec![LayerState {
                z_index: 0,
                id: 3,
                generation: 9,
                size: (40.0, 40.0),
                commands: vec![],
                motions: vec![offset, scale],
                clip: Some(crate::protocol::Rect {
                    x: 15.0,
                    y: 0.0,
                    w: 15.0,
                    h: 40.0,
                }),
                interactive: true,
                regions: vec![HitRegion {
                    id: 7,
                    x: 0.0,
                    y: 0.0,
                    w: 20.0,
                    h: 20.0,
                    radius: 0.0,
                }],
            }],
        };
        let middle = start + Duration::from_millis(500);
        assert_eq!(scene.hit(16.0, 10.0, middle), Some((3, 9, 7)));
        assert_eq!(scene.hit(14.0, 10.0, middle), None); // 固定裁剪
        assert_eq!(scene.hit(31.0, 10.0, middle), None);
        let mut upper = scene.layers[0].clone();
        upper.id = 4;
        upper.generation = 10;
        scene.layers.push(upper);
        assert_eq!(scene.hit(16.0, 10.0, middle), Some((4, 10, 7)));
        scene.layers[0].z_index = 1;
        assert_eq!(
            scene.ordered().iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![4, 3]
        );
        assert_eq!(scene.hit(16.0, 10.0, middle), Some((3, 9, 7)));
        scene.layers[0].z_index = -1;
        assert_eq!(scene.hit(16.0, 10.0, middle), Some((4, 10, 7)));
        scene.layers.pop();
        scene.layers[0].interactive = false;
        assert_eq!(scene.hit(16.0, 10.0, middle), None);
        scene.layers[0].interactive = true;
        scene.layers[0].motions[1].to = 0.0;
        assert_eq!(scene.hit(16.0, 10.0, middle), None);
    }

    #[test]
    fn frame_transactions_rollback_traps_and_distinguish_empty_submission() {
        let bytes = wat::parse_str(
            r#"(module
          (import "weasel_v2" "set_size" (func $size (param f32 f32)))
          (import "weasel_v2" "fill_rect" (func $rect (param f32 f32 f32 f32 i32)))
          (import "weasel_v2" "hit_region" (func $hit (param i32 f32 f32 f32 f32 f32)))
          (import "weasel_v2" "push_clip" (func $clip (param f32 f32 f32 f32)))
          (memory (export "memory") 1 1)
          (func (export "theme_abi_version") (result i32) i32.const 2)
          (func (export "theme_capabilities") (result i32) i32.const 0)
          (func (export "theme_create") (param i32 i32) (result i32) i32.const 0)
          (func (export "theme_event") (param i32 i32 f32 f32 f64) (result i32)
            local.get 0 i32.eqz if
              f32.const 100 f32.const 40 call $size
              f32.const 0 f32.const 0 f32.const 100 f32.const 40 i32.const -1 call $rect
              i32.const 7 f32.const 10 f32.const 10 f32.const 20 f32.const 20 f32.const 0 call $hit
              i32.const 1 return
            end
            local.get 1 i32.eqz if
              f32.const 999 f32.const 999 call $size unreachable
            end
            local.get 1 i32.const 1 i32.eq if
              f32.const 999 f32.const 999 call $size i32.const 0 return
            end
            local.get 1 i32.const 4 i32.eq if
              f32.const 0 f32.const 0 f32.const 10 f32.const 10 call $clip
            end
            i32.const 1))"#,
        )
        .unwrap();
        let mut rt = WasmRuntime::new(&bytes).unwrap();
        rt.render(&CandidateView::default()).unwrap();
        assert_eq!(rt.take_frame().unwrap().len(), 1);
        assert_eq!(rt.hit_test(15.0, 15.0), 7);
        assert!(rt.mouse(crate::protocol::MOUSE_DOWN, 15.0, 15.0).is_err());
        assert_eq!(rt.size(), (100.0, 40.0));
        assert_eq!(rt.hit_test(15.0, 15.0), 7);
        assert!(rt.take_frame().is_none());
        // Keep可以丢弃本次暂存的展示变更，不是隐式空帧。
        rt.mouse(crate::protocol::MOUSE_MOVE, 15.0, 15.0).unwrap();
        assert_eq!(rt.size(), (100.0, 40.0));
        assert!(rt.take_frame().is_none());
        // Present前验证状态栈，不能提交未闭合裁剪。
        assert!(rt.mouse(crate::protocol::MOUSE_CANCEL, 15.0, 15.0).is_err());
        assert_eq!(rt.hit_test(15.0, 15.0), 7);
        rt.mouse(crate::protocol::MOUSE_UP, 15.0, 15.0).unwrap();
        assert_eq!(rt.take_frame(), Some(Vec::new()));
    }

    #[test]
    fn abi_and_execution_limits_fail_as_errors() {
        assert!(
            WasmRuntime::new(&module("i32.const 0", 99))
                .unwrap_err()
                .contains("ABI")
        );
        let mut rt =
            WasmRuntime::new(&module("(loop $spin (br $spin)) i32.const 0", ABI_VERSION)).unwrap();
        assert!(
            rt.render(&CandidateView::default())
                .unwrap_err()
                .contains("fuel")
        );
        let mut state = HostState::default();
        assert!(state.charge(1024 * 1024 + 1).is_err());
    }

    #[test]
    fn declarations_and_metadata_are_independent_and_bounded() {
        // 元数据是纯数据，构造带自定义段的最小模块，不执行default函数。
        let make = |sections: &[&str], caps: &str, memory: &str| {
            let sections = sections
                .iter()
                .map(|s| {
                    let escaped = s.bytes().map(|b| format!("\\{b:02x}")).collect::<String>();
                    format!("(@custom \"weasel.settings\" \"{escaped}\")")
                })
                .collect::<String>();
            wat::parse_str(format!(r#"(module {sections}
                (import "weasel_v2" "request_frame" (func $host))
                (memory (export "memory") {memory} 1)
                (func (export "theme_abi_version") (result i32) i32.const 2)
                (func (export "theme_capabilities") (result i32) {caps})
                (func (export "theme_create") (param i32 i32) (result i32) i32.const 0)
                (func (export "theme_event") (param i32 i32 f32 f32 f64) (result i32) i32.const 0))"#)).unwrap()
        };
        let metadata = r#"{"formatVersion":1,"defaults":{"nested":{"a":1,"b":2}},"richschema":{"formatVersion":1,"schema":{"type":"object"}}}"#;
        let mut state = HostState::default();
        state.options = serde_json::json!({"nested":{"a":3}});
        let mut rt = WasmRuntime::with_state(&make(&[metadata], "i32.const 1", ""), state).unwrap();
        assert_eq!(
            rt.store.data().options,
            serde_json::json!({"nested":{"a":3,"b":2}})
        );
        rt.configure(true).unwrap();
        assert_eq!(
            WasmRuntime::new(&make(&[], "i32.const 0", ""))
                .unwrap()
                .store
                .data()
                .options,
            serde_json::json!({})
        );
        assert!(WasmRuntime::new(&make(&[metadata, metadata], "i32.const 0", "")).is_err());
        assert!(WasmRuntime::new(&make(&["not json"], "i32.const 0", "")).is_err());
        assert!(WasmRuntime::new(&make(&[], "i32.const 4", "")).is_err());
        assert!(
            WasmRuntime::new(&make(&[], "call $host i32.const 0", ""))
                .unwrap_err()
                .contains("before theme creation")
        );
        assert!(WasmRuntime::new(&make(&[], "i32.const 0", "i64")).is_err());
    }

    #[test]
    fn decoration_only_frames_preserve_main_and_rollback_failed_changes() {
        let bytes = wat::parse_str(
            r#"(module
          (import "weasel_v2" "set_size" (func $size (param f32 f32)))
          (import "weasel_v2" "fill_rect" (func $rect (param f32 f32 f32 f32 i32)))
          (import "weasel_v2" "hit_region" (func $hit (param i32 f32 f32 f32 f32 f32)))
          (import "weasel_v2" "layer_content" (func $layer (param i32 f32 f32)))
          (import "weasel_v2" "layer_set" (func $set (param i32 i32 f32)))
          (import "weasel_v2" "request_frame" (func $frame))
          (import "weasel_v2" "cancel_wakeup" (func $cancel))
          (memory (export "memory") 1 1)
          (func (export "theme_abi_version") (result i32) i32.const 2)
          (func (export "theme_capabilities") (result i32) i32.const 0)
          (func (export "theme_create") (param i32 i32) (result i32) i32.const 0)
          (func (export "theme_event") (param i32 i32 f32 f32 f64) (result i32)
            local.get 0 i32.eqz if
              f32.const 100 f32.const 40 call $size
              f32.const 0 f32.const 0 f32.const 100 f32.const 40 i32.const -1 call $rect
              i32.const 7 f32.const 0 f32.const 0 f32.const 100 f32.const 40 f32.const 0 call $hit
              i32.const 1 f32.const 10 f32.const 10 call $layer
              f32.const 0 f32.const 0 f32.const 10 f32.const 10 i32.const -1 call $rect
              i32.const 0 f32.const 0 f32.const 0 call $layer
              call $frame i32.const 1 return
            end
            i32.const 1 i32.const 0 f32.const 0.5 call $set
            call $cancel
            local.get 1 i32.eqz if unreachable end
            local.get 1 i32.const 1 i32.eq if i32.const 0 return end
            i32.const 1))"#,
        )
        .unwrap();
        let mut rt = WasmRuntime::new(&bytes).unwrap();
        rt.render(&CandidateView::default()).unwrap();
        let main = rt.take_frame().unwrap();
        assert!(rt.take_frame_request().deadline.is_some());
        assert_eq!(rt.layers().layers[0].commands.len(), 1);
        assert!(rt.mouse(crate::protocol::MOUSE_DOWN, 1.0, 1.0).is_err());
        assert!(!rt.take_frame_request().cancel);
        assert!(rt.layers().layers[0].motions.is_empty());
        rt.mouse(crate::protocol::MOUSE_MOVE, 1.0, 1.0).unwrap();
        assert!(rt.take_frame_request().cancel); // Keep retains valid nonvisual side effects.
        assert!(rt.layers().layers[0].motions.is_empty());
        rt.mouse(crate::protocol::MOUSE_UP, 1.0, 1.0).unwrap();
        assert_eq!(rt.take_frame(), Some(main));
        assert!(!rt.main_updated());
        assert_eq!(rt.hit_test(1.0, 1.0), 7);
        assert_eq!(rt.layers().layers[0].motions[0].to, 0.5);
        let _ = rt.hide();
        assert!(rt.layers().layers.is_empty());
    }

    #[test]
    fn typed_queries_preserve_unicode_integers_and_check_memory() {
        let bytes = wat::parse_str(
            r#"(module
          (import "weasel_v2" "view_i64" (func $integer (param i32 i32) (result i64)))
          (import "weasel_v2" "data_string" (func $string (param i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1 1)
          (data (i32.const 0) "/content_id")
          (data (i32.const 32) "/label")
          (func (export "theme_abi_version") (result i32) i32.const 2)
          (func (export "theme_capabilities") (result i32) i32.const 0)
          (func (export "theme_create") (param i32 i32) (result i32) i32.const 0)
          (func $render (result i32)
            i32.const 128 i32.const 0 i32.const 0 call $integer i64.store
            i32.const 1 i32.const 32 i32.const 6 i32.const 256 i32.const 32 call $string drop
            i32.const 0)
          (func $mouse (param i32 f32 f32)
            i32.const 1 i32.const 32 i32.const 6 i32.const 65535 i32.const 32 call $string drop)
          (func (export "theme_event") (param i32 i32 f32 f32 f64) (result i32)
            local.get 0 i32.eqz if (result i32) call $render else
            i32.const 0 f32.const 0 f32.const 0 call $mouse i32.const 0 end)
          (func (export "frame") (param f64))
          (func (export "hide"))
          (func (export "refresh") (param i32)))"#,
        )
        .unwrap();
        let mut state = HostState::default();
        state.options = serde_json::json!({"label":"你好😀"});
        let mut rt = WasmRuntime::with_state(&bytes, state).unwrap();
        rt.render(&CandidateView {
            content_id: u64::MAX,
            ..Default::default()
        })
        .unwrap();
        let memory = rt.instance.get_memory(&mut rt.store, MEMORY).unwrap();
        assert_eq!(&memory.data(&rt.store)[128..136], &u64::MAX.to_le_bytes());
        assert_eq!(&memory.data(&rt.store)[256..266], "你好😀".as_bytes());
        assert!(rt.mouse(0, 0.0, 0.0).is_err());
    }

    #[test]
    #[ignore = "build theme-weaselui first with npm run build"]
    fn weaselui_defaults_overlay_and_probe() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("theme-weaselui/build/weaselui.wasm"),
        )
        .unwrap();
        let mut state = HostState::default();
        state.options = serde_json::json!({"color":{"hilited_candidate_back":"#123456"}});
        let mut rt = WasmRuntime::with_state(&bytes, state).unwrap();
        rt.configure(true).unwrap(); // TODO: guest implements external preedit later.
        assert_eq!(rt.store.data().options["layout"]["margin_x"], 12);
        rt.init(0, false).unwrap();
        rt.render(&CandidateView {
            visible: true,
            selected_index: 0,
            items: vec![CandidateItem {
                primary_text: "你好".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        assert!(rt.take_commands().iter().any(|c| matches!(
            c,
            DrawCommand::FillRoundedRect {
                color: 0xff123456,
                ..
            }
        )));
    }

    #[test]
    #[ignore = "build theme-glass for wasm32-unknown-unknown --release first"]
    fn glass_guest_material_contract() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("theme-glass/target/wasm32-unknown-unknown/release/glass.wasm"),
        )
        .unwrap();
        let mut rt = WasmRuntime::with_state(&bytes, HostState::default()).unwrap();
        assert!(rt.configure(true).is_err());
        rt.configure(false).unwrap();
        rt.init(0, false).unwrap();
        rt.render(&CandidateView {
            visible: true,
            items: vec![CandidateItem {
                primary_text: "玻璃".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let style = rt.backdrop_style();
        assert!(style.enabled);
        assert_eq!(style.blur_sigma, 19.0);
        assert!(
            (style.backdrop_balance + style.afterglow_balance + style.color_balance - 1.0).abs()
                < 0.001
        );
        assert_eq!(rt.panel_style().corner_radius, 10.0);
        assert!(!rt.take_commands().is_empty());
    }

    #[test]
    #[ignore = "build theme-statusbar with npm run build first"]
    fn statusbar_resident_mode_contract() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("theme-statusbar/build/statusbar.wasm"),
        )
        .unwrap();
        let mut runtime = WasmRuntime::new(&bytes).unwrap();
        runtime.configure(true).unwrap();
        assert!(runtime.resident());
        runtime.init(crate::protocol::MODE_LIVE, false).unwrap();

        let mut view = CandidateView {
            active: true,
            ascii_mode: Some(false),
            ..Default::default()
        };
        runtime.render(&view).unwrap();
        assert!(runtime.visible());
        assert!(matches!(
            runtime.placement(),
            crate::protocol::PlacementStyle::Fixed { .. }
        ));
        assert!(!runtime.take_commands().is_empty());
        runtime
            .mouse(crate::protocol::MOUSE_DOWN, 20.0, 10.0)
            .unwrap();
        assert!(runtime.take_drag_request());

        view.ascii_mode = Some(true);
        runtime.render(&view).unwrap();
        assert!(!runtime.visible());
    }

    #[test]
    #[ignore = "build SDK first with npm run asbuild:release"]
    fn sdk_sample_theme_end_to_end() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk-as/build/release.wasm");
        let bytes = std::fs::read(path).expect("build SDK first");
        let mut state = HostState::default();
        state.options = serde_json::json!({"fontSize": 20});
        let mut rt = WasmRuntime::with_state(&bytes, state).unwrap();
        rt.init(0, true).unwrap();
        let view = CandidateView {
            visible: true,
            items: vec![CandidateItem {
                primary_text: "你好😀".into(),
                secondary_text: "ni hao".into(),
                enabled: true,
            }],
            ..Default::default()
        };
        rt.render(&view).unwrap();
        assert_eq!(rt.size().1, 42.0);
        assert!(
            rt.take_commands()
                .iter()
                .any(|c| matches!(c, DrawCommand::Layout { .. }))
        );
        rt.mouse(crate::protocol::MOUSE_DOWN, 10.0, 21.0).unwrap();
        assert!(rt.take_actions().is_empty());
        rt.mouse(crate::protocol::MOUSE_UP, 10.0, 21.0).unwrap();
        assert_eq!(rt.take_actions(), vec![(Action::Item as i32, 0)]);
        rt.mouse(crate::protocol::MOUSE_DOWN, 10.0, 21.0).unwrap();
        rt.mouse(crate::protocol::MOUSE_LEAVE, 0.0, 0.0).unwrap();
        rt.mouse(crate::protocol::MOUSE_UP, 10.0, 21.0).unwrap();
        assert!(rt.take_actions().is_empty());
        rt.hide().unwrap();
        assert!(rt.store.data().view.is_none());
    }

    #[test]
    #[ignore = "build and package theme-orbit first; see its README"]
    fn orbit_animation_and_interaction_contract() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("theme-orbit/target/wasm32-unknown-unknown/release/orbit.wasm"),
        )
        .unwrap();
        let mut rt = WasmRuntime::new(&bytes).unwrap();
        rt.configure(true).unwrap();
        assert_eq!(rt.store.data().options["fontSize"], 16);
        rt.init(0, false).unwrap();
        let mut view = CandidateView {
            visible: true,
            active: true,
            items: vec![CandidateItem {
                primary_text: "你好😀".into(),
                secondary_text: "ni hao".into(),
                enabled: true,
            }],
            preedit: Some(crate::theme_api::Preedit {
                text: "你好😀ni".into(),
                cursor: 4,
            }),
            ..Default::default()
        };
        rt.render(&view).unwrap();
        assert_eq!(rt.layers().layers.len(), 3);
        assert!(rt.size().0 > rt.anchor_rect().w);
        let main = rt.take_commands();
        assert!(!main.is_empty());
        let wake = rt.take_frame_request().merge(None).unwrap();
        // 提前唤醒不能提交空帧；到期只改装饰，原文字与命中保持。
        rt.frame(wake - 1.0).unwrap();
        assert!(rt.take_frame().is_none());
        let region = rt
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 3)
            .unwrap()
            .regions[0]
            .clone();
        rt.frame(wake + 1.0).unwrap();
        assert_eq!(rt.take_commands(), main);
        assert_eq!(rt.measure_calls(), 0);
        assert!(!rt.main_updated());
        let moon_motion = |rt: &WasmRuntime| {
            *rt.layers()
                .layers
                .iter()
                .find(|l| l.id == 1)
                .unwrap()
                .motions
                .iter()
                .find(|m| m.property == crate::layers::LayerProperty::OffsetY)
                .unwrap()
        };
        let before_page = moon_motion(&rt);
        view.page_start = 5;
        rt.render(&view).unwrap();
        assert_eq!(
            rt.layers()
                .ordered()
                .iter()
                .map(|l| l.id)
                .collect::<Vec<_>>(),
            vec![4, 3, 1, 2]
        );
        assert_eq!(moon_motion(&rt).revision, before_page.revision);
        let page_generation = rt
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 4)
            .unwrap()
            .generation;
        rt.render(&view).unwrap();
        assert_eq!(
            rt.layers()
                .layers
                .iter()
                .find(|l| l.id == 4)
                .unwrap()
                .generation,
            page_generation
        );
        // 新页命中位置与滑入位置一致，旧页原位置不接受点击。
        assert_eq!(rt.hit_test(region.x + 1.0, region.y + 10.0), -1);
        // 整行从裁剪区外进入；等待原生时间线结束，再验证新页点击与 hover。
        std::thread::sleep(std::time::Duration::from_millis(320));
        rt.frame(now_ms()).unwrap();
        let moving = &rt
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 3)
            .unwrap()
            .regions[0];
        let (x, y) = (moving.x + moving.w / 2.0, moving.y + 10.0);
        let normal = rt
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 3)
            .unwrap()
            .commands
            .clone();
        rt.mouse(crate::protocol::MOUSE_MOVE, x, y).unwrap();
        assert_ne!(
            rt.layers()
                .layers
                .iter()
                .find(|l| l.id == 3)
                .unwrap()
                .commands,
            normal
        );
        rt.mouse(crate::protocol::MOUSE_DOWN, x, y).unwrap();
        rt.mouse(crate::protocol::MOUSE_UP, x, y).unwrap();
        assert_eq!(rt.take_actions(), vec![(Action::Item as i32, 0)]);
        rt.mouse(crate::protocol::MOUSE_DOWN, x, y).unwrap();
        rt.render(&view).unwrap();
        rt.mouse(crate::protocol::MOUSE_UP, x, y).unwrap();
        assert!(rt.take_actions().is_empty());
        rt.frame(wake + 1000.0).unwrap();
        assert!(rt.layers().layers.iter().all(|l| l.id != 4));
        assert_eq!(rt.measure_calls(), 0);
        rt.refresh(true).unwrap();
        rt.hide().unwrap();
        assert!(rt.layers().layers.is_empty());
        assert!(rt.take_frame_request().merge(None).is_none());
        view.preedit = None;
        rt.render(&view).unwrap();
        assert_eq!(rt.layers().layers.len(), 3);
        // 关闭主题自身动画：仍绘制装饰，但没有时间驱动任务。
        let mut state = HostState::default();
        state.options = serde_json::json!({"animations": false, "fontSize": 24});
        let mut quiet = WasmRuntime::with_state(&bytes, state).unwrap();
        quiet.configure(true).unwrap();
        quiet.init(0, true).unwrap();
        quiet.render(&view).unwrap();
        assert!(quiet.take_frame_request().merge(None).is_none());
        view.page_start = 10;
        quiet.render(&view).unwrap();
        assert!(quiet.layers().layers.iter().all(|l| l.id != 4));
        let normal = quiet
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 3)
            .unwrap()
            .commands
            .clone();
        let hit = &quiet
            .layers()
            .layers
            .iter()
            .find(|l| l.id == 3)
            .unwrap()
            .regions[0];
        let (hx, hy) = (hit.x + hit.w / 2.0, hit.y + hit.h / 2.0);
        quiet.mouse(crate::protocol::MOUSE_MOVE, hx, hy).unwrap();
        assert_ne!(
            quiet
                .layers()
                .layers
                .iter()
                .find(|l| l.id == 3)
                .unwrap()
                .commands,
            normal
        );
        quiet.mouse(crate::protocol::MOUSE_LEAVE, hx, hy).unwrap();
        assert_eq!(
            quiet
                .layers()
                .layers
                .iter()
                .find(|l| l.id == 3)
                .unwrap()
                .commands,
            normal
        );
        assert!(
            quiet
                .layers()
                .layers
                .iter()
                .flat_map(|l| &l.motions)
                .all(|m| m.operation != crate::layers::LayerOperation::Animate)
        );
        view.items = (0..64)
            .map(|i| CandidateItem {
                primary_text: format!("很长的候选文字{i}"),
                secondary_text: format!("comment-{i}"),
                enabled: true,
            })
            .collect();
        quiet.render(&view).unwrap();
        assert_eq!(
            quiet
                .layers()
                .layers
                .iter()
                .find(|l| l.id == 3)
                .unwrap()
                .regions
                .len(),
            64
        );
        quiet.render(&view).unwrap();
        assert_eq!(quiet.measure_calls(), 0);
    }
}
impl Drop for WasmRuntime {
    /// 若主题已进入初始化生命周期，尽力调用一次有界的销毁导出。
    fn drop(&mut self) {
        // 清理也受fuel/host配额约束；trap不能阻止host回收实例资源。
        if self.initialized {
            if let Some(destroy) = self.destroy_fn.take() {
                if self.set_fuel(EVENT_FUEL).is_ok() {
                    let _ = destroy.call(&mut self.store, ());
                }
            }
        }
    }
}
