//! Wasmtime 运行时封装：持有 Store/Instance，注册 host 导入函数，
//! 提供类型化的导出调用入口，并把 wasm trap 收敛为 `Err(String)`。
//!
//! `Engine` 为进程级单例（OnceLock）；模块目前每次创建时重新编译，
//! 确保预览刷新能重新加载磁盘上的主题；不反序列化不可信的本地编译产物。
//! 每个 [`WasmRuntime`] 拥有独立的 `Store`（绑定创建它的 UI 线程，非 Send）。
//!
//! `measure_text` 委托给 [`HostState::measure`] 闭包：默认按每字符 8 DIP
//! 估算（测试用），window 后端接入时替换为 canvas.rs 的 DirectWrite 实测。

use std::{
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use wasmtime::{
    Caller, Config, Engine, FuncType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc, Val, ValType,
};

use crate::protocol::{
    ABI_VERSION, ACTION_EMOJI, ACTION_ITEM, DrawCommand, ERR_OK, EXPORT_ABI_VERSION, EXPORT_FRAME,
    EXPORT_HIDE, EXPORT_INIT, EXPORT_MOUSE, EXPORT_REFRESH, EXPORT_RENDER, IMPORT_ABORT,
    IMPORT_ABORT_MODULE, IMPORT_DRAW_TEXT, IMPORT_FILL_RECT, IMPORT_FILL_ROUNDED_RECT, IMPORT_LOG,
    IMPORT_MEASURE_TEXT, IMPORT_MODULE, IMPORT_REQUEST_FRAME, IMPORT_SEND_ACTION, IMPORT_SET_PANEL,
    IMPORT_SET_SIZE, IMPORT_STROKE_RECT, IMPORT_TIME_MS, MAX_STRING_BYTES, MEMORY, PanelStyle,
};

/// 线性内存上限（字节）：主题持有视图副本与布局状态，128 MiB 远超正常需求，
/// 超限的 `memory.grow` 直接 trap（见 `trap_on_grow_failure`）。
pub const MAX_MEMORY_BYTES: usize = 128 * 1024 * 1024;
/// `set_size` 的单边上限（DIP）：约等于任何屏幕的最大物理尺寸。
pub const MAX_DIM_DIP: f32 = 8192.0;
/// 单条 `draw_text` 的字节上限：防主题塞入巨型文本撑爆 DirectWrite 布局。
pub const MAX_TEXT_BYTES: i32 = 65_536;

// 每次导出的指令预算（wasmtime fuel）：超出即 trap，trap 被收敛为 Err。
/// start 函数（如 AssemblyScript 静态初始化）在实例化时执行，单列预算。
const INSTANTIATE_FUEL: u64 = 10_000_000;
const INIT_FUEL: u64 = 1_000_000;
const RENDER_FUEL: u64 = 10_000_000;
const EVENT_FUEL: u64 = 1_000_000;

/// 进程级 Wasmtime 引擎（启用 fuel 计量）。
static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

fn engine() -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| {
            let mut config = Config::default();
            config.consume_fuel(true);
            Engine::new(&config).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// 主题实例的 host 侧状态，作为 `Store` 数据持有。
pub struct HostState {
    pub view: serde_json::Value,
    pub options: serde_json::Value,
    pub settings: serde_json::Value,
    text_glow: (f32, u32),
    host_calls: usize,
    text_bytes: usize,
    accept_actions: bool,
    /// 资源限额（经 `Store::limiter` 生效）：内存/表/实例增长上限。
    pub limits: StoreLimits,
    /// 当前帧的绘制命令（host 在 WM_PAINT 回放）。
    pub commands: Vec<DrawCommand>,
    /// 主题最近一次声明的内容尺寸（DIP）。
    pub size: (f32, f32),
    pub panel_style: PanelStyle,
    pub backdrop_style: crate::protocol::BackdropStyle,
    /// 主题请求了下一动画帧。
    pub frame_requested: bool,
    /// 主题请求的高层动作 `(action, index)`。
    pub actions: Vec<(i32, i32)>,
    /// 诊断文本（`log` 导入与 host 侧警告）。
    pub notes: Vec<String>,
    /// `measure_text` 调用计数（测试用）。
    pub measure_calls: u32,
    /// 文本测量委托：window 后端注入 DirectWrite 实测（canvas.rs），
    /// 默认按每字符 8 DIP 估算。非 Send：绑定 UI 线程。
    pub set_font: Box<dyn FnMut(i32, &str)>,
    pub line_height: Box<dyn FnMut(i32, f32) -> f32>,
    pub measure: Box<dyn FnMut(&str, i32, f32) -> f32>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            view: serde_json::Value::Null,
            options: serde_json::json!({}),
            settings: serde_json::json!({}),
            text_glow: (0.0, 0),
            host_calls: 0,
            text_bytes: 0,
            accept_actions: false,
            limits: default_limits(),
            commands: Vec::new(),
            size: (0.0, 0.0),
            panel_style: PanelStyle::default(),
            backdrop_style: Default::default(),
            frame_requested: false,
            actions: Vec::new(),
            notes: Vec::new(),
            measure_calls: 0,
            set_font: Box::new(|_, _| {}),
            line_height: Box::new(|_, size| size * 1.4),
            measure: Box::new(|text, _font, _size| (text.len().max(1)) as f32 * 8.0),
        }
    }
}

impl HostState {
    /// Fuel only bounds guest instructions. Bound expensive native work separately.
    pub(crate) fn charge(&mut self, bytes: usize) -> wasmtime::Result<()> {
        self.host_calls += 1;
        self.text_bytes = self.text_bytes.saturating_add(bytes);
        if self.host_calls > 4096 || self.text_bytes > 1024 * 1024 {
            return Err(wasmtime::format_err!("theme host-call budget exceeded"));
        }
        Ok(())
    }

    fn draw(&mut self, command: DrawCommand) -> wasmtime::Result<()> {
        if !command.is_finite() {
            return Err(wasmtime::format_err!("non-finite draw coordinates"));
        }
        if self.commands.len() >= 1024 {
            return Err(wasmtime::format_err!("theme draw-command limit exceeded"));
        }
        self.commands.push(command);
        Ok(())
    }
}

/// 默认资源限额：内存 128 MiB（超限 grow 直接 trap）、表/实例有界。
fn default_limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .memories(16)
        .tables(256)
        .table_elements(1 << 20)
        .instances(4)
        .trap_on_grow_failure(true)
        .build()
}

/// 读取线性内存中的 UTF-8 字符串；越界或负长度返回 `None`。
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

fn set_font(
    mut caller: Caller<'_, HostState>,
    slot: i32,
    ptr: i32,
    len: i32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(len.max(0) as usize)?;
    if !(0..4).contains(&slot) || !(1..=256).contains(&len) {
        return Err(wasmtime::format_err!("invalid font"));
    }
    let family = read_wasm_string(&mut caller, ptr, len)
        .ok_or_else(|| wasmtime::format_err!("invalid font memory"))?;
    if family.contains('\0') {
        return Err(wasmtime::format_err!("invalid font name"));
    }
    (caller.data_mut().set_font)(slot, &family);
    Ok(())
}
fn line_height(mut caller: Caller<'_, HostState>, slot: i32, size: f32) -> wasmtime::Result<f32> {
    caller.data_mut().charge(0)?;
    if !size.is_finite() || !(4.0..=512.0).contains(&size) {
        return Err(wasmtime::format_err!("invalid font size"));
    }
    Ok((caller.data_mut().line_height)(slot, size))
}

fn set_corner_radius(mut caller: Caller<'_, HostState>, radius: f32) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    if !radius.is_finite() || !(0.0..=4096.0).contains(&radius) {
        return Err(wasmtime::format_err!("invalid window corner radius"));
    }
    caller.data_mut().panel_style.corner_radius = radius;
    Ok(())
}

fn set_panel(
    mut caller: Caller<'_, HostState>,
    radius: f32,
    shadow_radius: f32,
    offset_x: f32,
    offset_y: f32,
    color: i32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
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
    caller.data_mut().panel_style = PanelStyle {
        corner_radius: radius,
        shadow_radius,
        offset_x,
        offset_y,
        color: color as u32,
    };
    Ok(())
}

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
    caller.data_mut().charge(0)?;
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

fn measure_text(
    mut caller: Caller<'_, HostState>,
    ptr: i32,
    len: i32,
    font: i32,
    size: f32,
) -> wasmtime::Result<f32> {
    caller.data_mut().charge(len.max(0) as usize)?;
    if !(0..=MAX_TEXT_BYTES).contains(&len) || !size.is_finite() || !(4.0..=512.0).contains(&size) {
        return Err(wasmtime::format_err!("invalid text measurement arguments"));
    }
    let text = read_wasm_string(&mut caller, ptr, len);
    let st = caller.data_mut();
    st.measure_calls = st.measure_calls.saturating_add(1);
    if st.measure_calls > 256 {
        return Err(wasmtime::format_err!("theme measurement limit exceeded"));
    }
    match text {
        Some(text) => Ok((st.measure)(&text, font, size)),
        None => {
            st.notes.push("measure_text: text out of bounds".into());
            Ok(0.0)
        }
    }
}

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

fn draw_text(
    mut caller: Caller<'_, HostState>,
    text: i32,
    len: i32,
    x: f32,
    y: f32,
    font: i32,
    size: f32,
    color: u32,
) -> wasmtime::Result<()> {
    caller.data_mut().charge(len.max(0) as usize)?;
    if !(0..=MAX_TEXT_BYTES).contains(&len) || !size.is_finite() || !(4.0..=512.0).contains(&size) {
        return Err(wasmtime::format_err!("invalid draw_text arguments"));
    }
    let text = read_wasm_string(&mut caller, text, len)
        .ok_or_else(|| wasmtime::format_err!("invalid draw_text memory or UTF-8"))?;
    let st = caller.data_mut();
    st.draw(DrawCommand::Text {
        x,
        y,
        font,
        size,
        color,
        text,
        glow: st.text_glow,
    })
}

fn set_size(mut caller: Caller<'_, HostState>, w: f32, h: f32) -> wasmtime::Result<()> {
    caller.data_mut().charge(0)?;
    let st = caller.data_mut();
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        st.notes
            .push(format!("set_size ignored: invalid size ({w}, {h})"));
        return Ok(());
    }
    st.size = (w.min(MAX_DIM_DIP), h.min(MAX_DIM_DIP));
    Ok(())
}

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
    if !matches!(action, ACTION_ITEM..=ACTION_EMOJI) || index < 0 {
        st.notes.push(format!(
            "send_action ignored: action={action} index={index}"
        ));
        return Ok(());
    }
    st.actions.push((action, index));
    Ok(())
}

fn request_frame(mut caller: Caller<'_, HostState>) -> wasmtime::Result<()> {
    caller.data_mut().frame_requested = true;
    Ok(())
}

fn log(caller: Caller<'_, HostState>, msg: i32, len: i32) -> wasmtime::Result<()> {
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

/// AssemblyScript passes UTF-16 object pointers, followed by line/column, not lengths.
/// Avoid depending on its managed object layout; preserve source coordinates.
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

pub(crate) fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn register_imports(linker: &mut Linker<HostState>) -> Result<(), String> {
    let register = |result: wasmtime::Result<&mut Linker<HostState>>| -> Result<(), String> {
        result
            .map(|_| ())
            .map_err(|e| format!("failed to register imports: {e}"))
    };
    register(linker.func_wrap(IMPORT_MODULE, "set_corner_radius", set_corner_radius))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_PANEL, set_panel))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        "set_text_glow",
        |mut caller: Caller<'_, HostState>, radius: f32, color: u32| -> wasmtime::Result<()> {
            caller.data_mut().charge(0)?;
            if !radius.is_finite() || !(0.0..=4.0).contains(&radius) {
                return Err(wasmtime::format_err!("invalid text glow radius"));
            }
            caller.data_mut().text_glow = (radius, color);
            Ok(())
        },
    ))?;
    register(linker.func_wrap(
        IMPORT_MODULE,
        crate::protocol::IMPORT_SET_BACKDROP,
        set_backdrop,
    ))?;
    register(linker.func_wrap(IMPORT_MODULE, "set_font", set_font))?;
    register(linker.func_wrap(IMPORT_MODULE, "line_height", line_height))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_MEASURE_TEXT, measure_text))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_FILL_RECT, fill_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_FILL_ROUNDED_RECT, fill_rounded_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_STROKE_RECT, stroke_rect))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_DRAW_TEXT, draw_text))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SET_SIZE, set_size))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_SEND_ACTION, send_action))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_REQUEST_FRAME, request_frame))?;
    // time_ms 无参数：func_wrap 不支持零参闭包，用 func_new 显式给出类型。
    register(linker.func_new(
        IMPORT_MODULE,
        IMPORT_TIME_MS,
        FuncType::new(linker.engine(), [], [ValType::F64]),
        |_caller, _params, results| {
            // Val::F64 携带 IEEE 754 位模式（u64）。
            results[0] = Val::F64(now_ms().to_bits());
            Ok(())
        },
    ))?;
    register(linker.func_wrap(IMPORT_MODULE, IMPORT_LOG, log))?;
    // AssemblyScript 运行时导入 env.abort（断言/panic 入口）；缺失则实例化失败。
    register(linker.func_wrap(IMPORT_ABORT_MODULE, IMPORT_ABORT, abort))?;
    crate::data::register(linker).map_err(err_string)?;
    Ok(())
}

// ── 运行时 ───────────────────────────────────────────────────────────

/// 一个已实例化的主题模块及其 host 状态。非 `Send`：绑定在创建它的 UI 线程。
pub struct WasmRuntime {
    store: Store<HostState>,
    /// 保持实例存活；导出句柄（TypedFunc/Memory）已独立引用 store 对象。
    #[allow(dead_code)]
    instance: wasmtime::Instance,
    init_fn: TypedFunc<(i32, i32), i32>,
    render_fn: TypedFunc<(), i32>,
    mouse_fn: TypedFunc<(i32, f32, f32), ()>,
    frame_fn: TypedFunc<f64, ()>,
    hide_fn: TypedFunc<(), ()>,
    refresh_fn: TypedFunc<i32, ()>,
}

impl std::fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmRuntime").finish()
    }
}

fn check_code(code: i32, what: &str) -> Result<(), String> {
    if code == ERR_OK {
        Ok(())
    } else {
        Err(format!("theme {what} failed with error code {code}"))
    }
}

/// 把 wasmtime 错误转为可读字符串。顶层 `Display` 仅输出 wasm backtrace；
/// trap 根因（如 fuel 耗尽、越界）在 source 链末端，遍历 `chain()` 一并拼入。
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
    /// 编译并实例化主题模块；校验全部导出与 `memory` 导出。
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        Self::with_state(bytes, HostState::default())
    }

    /// 以自定义 host 状态实例化（window 后端注入 DirectWrite 测量闭包）。
    pub fn with_state(bytes: &[u8], state: HostState) -> Result<Self, String> {
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
        let render_fn = instance
            .get_typed_func(&mut store, EXPORT_RENDER)
            .map_err(&export_error)?;
        let mouse_fn = instance
            .get_typed_func(&mut store, EXPORT_MOUSE)
            .map_err(&export_error)?;
        let frame_fn = instance
            .get_typed_func(&mut store, EXPORT_FRAME)
            .map_err(&export_error)?;
        let hide_fn = instance
            .get_typed_func(&mut store, EXPORT_HIDE)
            .map_err(&export_error)?;
        let refresh_fn = instance
            .get_typed_func(&mut store, EXPORT_REFRESH)
            .map_err(&export_error)?;
        Ok(Self {
            store,
            instance,
            init_fn,
            render_fn,
            mouse_fn,
            frame_fn,
            hide_fn,
            refresh_fn,
        })
    }

    /// 为下一次导出调用装载 fuel 预算；耗尽即 trap（收敛为 Err）。
    fn set_fuel(&mut self, budget: u64) -> Result<(), String> {
        let state = self.store.data_mut();
        state.host_calls = 0;
        state.text_bytes = 0;
        state.measure_calls = 0;
        state.commands.clear();
        state.actions.clear();
        state.frame_requested = false;
        state.accept_actions = false;
        self.store.set_fuel(budget).map_err(err_string)
    }

    /// Optional guest defaults; JSON parsing/overlay stays in the host.
    pub fn configure(&mut self, preedit: bool) -> Result<(), String> {
        self.set_fuel(INIT_FUEL)?;
        if self
            .instance
            .get_export(&mut self.store, "default_config")
            .is_some()
        {
            // Packed pointer in low 32 bits, UTF-8 byte length in high bits.
            let range = self
                .instance
                .get_typed_func::<(), i64>(&mut self.store, "default_config")
                .map_err(err_string)?
                .call(&mut self.store, ())
                .map_err(err_string)? as u64;
            let ptr = (range as u32) as usize;
            let len = (range >> 32) as usize;
            if len > 1024 * 1024 {
                return Err("theme defaults exceed 1 MiB".into());
            }
            let memory = self
                .instance
                .get_memory(&mut self.store, MEMORY)
                .ok_or("theme memory missing")?;
            let bytes = memory
                .data(&self.store)
                .get(ptr..ptr.checked_add(len).ok_or("defaults range overflow")?)
                .ok_or("theme defaults outside memory")?;
            let mut defaults: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
            if !defaults.is_object() {
                return Err("theme defaults must be an object".into());
            }
            weasel_common::settings::merge(&mut defaults, self.store.data().options.clone());
            if defaults.to_string().len() > 1024 * 1024 {
                return Err("merged theme config exceeds 1 MiB".into());
            }
            self.store.data_mut().options = defaults;
        }
        self.set_fuel(INIT_FUEL)?;
        if self
            .instance
            .get_export(&mut self.store, "probe_preedit")
            .is_some()
        {
            let code = self
                .instance
                .get_typed_func::<i32, i32>(&mut self.store, "probe_preedit")
                .map_err(err_string)?
                .call(&mut self.store, preedit as i32)
                .map_err(err_string)?;
            check_code(code, "preedit probe")?;
        } else if preedit {
            // TODO: reject modules without a preedit probe once guest support lands.
        }
        Ok(())
    }

    /// 调用 `init(mode, dark)`。
    pub fn init(&mut self, mode: i32, dark: bool) -> Result<(), String> {
        self.set_fuel(INIT_FUEL)?;
        let code = self
            .init_fn
            .call(&mut self.store, (mode, dark as i32))
            .map_err(err_string)?;
        check_code(code, "init")
    }

    /// Publish a host-owned typed snapshot. Guest queries only the fields it needs.
    pub fn render(&mut self, view: &crate::theme_api::CandidateView) -> Result<(), String> {
        self.store.data_mut().view = serde_json::to_value(view).map_err(|e| e.to_string())?;
        self.set_fuel(RENDER_FUEL)?;
        let code = self
            .render_fn
            .call(&mut self.store, ())
            .map_err(err_string)?;
        check_code(code, "render")
    }

    /// 转发鼠标事件（窗口局部 DIP 坐标）。
    pub fn mouse(&mut self, kind: i32, x: f32, y: f32) -> Result<(), String> {
        self.set_fuel(EVENT_FUEL)?;
        self.store.data_mut().accept_actions = matches!(
            kind,
            crate::protocol::MOUSE_DOWN | crate::protocol::MOUSE_UP
        );
        self.mouse_fn
            .call(&mut self.store, (kind, x, y))
            .map_err(err_string)
    }

    /// 触发动画帧回调。
    pub fn frame(&mut self, now_ms: f64) -> Result<(), String> {
        self.set_fuel(EVENT_FUEL)?;
        self.frame_fn
            .call(&mut self.store, now_ms)
            .map_err(err_string)
    }

    /// 通知主题隐藏。
    pub fn hide(&mut self) -> Result<(), String> {
        self.set_fuel(EVENT_FUEL)?;
        let result = self.hide_fn.call(&mut self.store, ()).map_err(err_string);
        self.store.data_mut().view = serde_json::Value::Null;
        self.store.data_mut().actions.clear();
        result
    }

    /// 通知主题外观变化。
    pub fn refresh(&mut self, dark: bool) -> Result<(), String> {
        self.set_fuel(EVENT_FUEL)?;
        self.refresh_fn
            .call(&mut self.store, dark as i32)
            .map_err(err_string)
    }

    /// 取走当前帧的全部绘制命令。
    pub fn take_commands(&mut self) -> Vec<DrawCommand> {
        std::mem::take(&mut self.store.data_mut().commands)
    }

    /// Latest native panel presentation declared by the guest.
    pub fn panel_style(&self) -> PanelStyle {
        self.store.data().panel_style
    }
    pub fn backdrop_style(&self) -> crate::protocol::BackdropStyle {
        self.store.data().backdrop_style
    }

    /// Compatibility accessor for the panel's corner radius in DIP.
    pub fn corner_radius(&self) -> f32 {
        self.panel_style().corner_radius
    }

    pub fn size(&self) -> (f32, f32) {
        self.store.data().size
    }

    /// 取走动画帧请求标志。
    pub fn take_frame_request(&mut self) -> bool {
        std::mem::take(&mut self.store.data_mut().frame_requested)
    }

    /// 取走本帧请求的高层动作 `(action, index)`。
    pub fn take_actions(&mut self) -> Vec<(i32, i32)> {
        std::mem::take(&mut self.store.data_mut().actions)
    }

    /// 取走诊断 notes。
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().notes)
    }

    /// `measure_text` 调用计数（测试用）。
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
          (func (export "abi_version") (result i32) i32.const {version})
          (func (export "init") (param i32 i32) (result i32) i32.const 0)
          (func (export "render") (result i32) {body})
          (func (export "mouse") (param i32 f32 f32))
          (func (export "frame") (param f64))
          (func (export "hide"))
          (func (export "refresh") (param i32)))"#
        ))
        .unwrap()
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
    fn typed_queries_preserve_unicode_integers_and_check_memory() {
        let bytes = wat::parse_str(
            r#"(module
          (import "weasel" "data_i64" (func $integer (param i32 i32 i32) (result i64)))
          (import "weasel" "data_string" (func $string (param i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1 1)
          (data (i32.const 0) "/content_id")
          (data (i32.const 32) "/label")
          (func (export "abi_version") (result i32) i32.const 1)
          (func (export "init") (param i32 i32) (result i32) i32.const 0)
          (func (export "render") (result i32)
            i32.const 128 i32.const 0 i32.const 0 i32.const 11 call $integer i64.store
            i32.const 1 i32.const 32 i32.const 6 i32.const 256 i32.const 32 call $string drop
            i32.const 0)
          (func (export "mouse") (param i32 f32 f32)
            i32.const 1 i32.const 32 i32.const 6 i32.const 65535 i32.const 32 call $string drop)
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
        assert!(rt.take_commands().iter().any(|c| matches!(c, DrawCommand::Text { text, size, .. } if text == "你好😀" && *size == 20.0)));
        rt.mouse(crate::protocol::MOUSE_DOWN, 10.0, 21.0).unwrap();
        assert!(rt.take_actions().is_empty());
        rt.mouse(crate::protocol::MOUSE_UP, 10.0, 21.0).unwrap();
        assert_eq!(rt.take_actions(), vec![(ACTION_ITEM, 0)]);
        rt.mouse(crate::protocol::MOUSE_DOWN, 10.0, 21.0).unwrap();
        rt.mouse(crate::protocol::MOUSE_LEAVE, 0.0, 0.0).unwrap();
        rt.mouse(crate::protocol::MOUSE_UP, 10.0, 21.0).unwrap();
        assert!(rt.take_actions().is_empty());
        rt.hide().unwrap();
        assert!(rt.store.data().view.is_null());
    }
}
