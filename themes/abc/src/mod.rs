//! 提供经典 ABC 风格的输入预编辑框与候选窗，绘制由 Direct2D/DirectWrite 完成，
//!
//! 窗口只呈现并转发候选快照中的交互事件；输入状态仍归 TIP 所有。原生窗口回调
//! 使用稳定的 `Rc<Window>` 分配承载状态，绘图资源在创建它们的 UI 线程上使用。
mod logic;

use crate::d2d_bindings::*;
use crate::theme_api::CandidateView;
use crate::{
    theme_api::EventSink,
    theme_api::{ThemeBackend, UiMode},
};
use logic::{Gesture, Hit, Layout, PAD, Recovery, Role, enabled, pixels};
use std::{
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};
use windows_strings::w;

const CLASS: windows_strings::PCWSTR = w!("Weasel.ThemeAbc.D2D");
const RETRY_TIMER: usize = 1;

/// 一组绑定到当前 UI 线程的 Direct2D/DirectWrite 资源。
///
/// 工厂和文本格式随窗口存活；HWND 渲染目标可在设备丢失后丢弃，并由
/// [`Graphics::ensure_target`] 按需重建。
struct Graphics {
    /// 创建 HWND 渲染目标的单线程 Direct2D 工厂。
    factory: ID2D1Factory,
    /// 创建文本布局的 DirectWrite 工厂。
    write: IDWriteFactory,
    /// 预编辑文本格式。
    text: IDWriteTextFormat,
    /// 候选窗底部标签格式。
    label: IDWriteTextFormat,
    /// 独立中英文模式提示窗使用的居中文字格式。
    mode_indicator: IDWriteTextFormat,
    /// 与当前 HWND 关联的渲染目标；设备失效时设为 `None`。
    target: Option<ID2D1HwndRenderTarget>,
}

impl Graphics {
    /// 在调用线程创建工厂与固定文本格式；底层 API 失败时原样返回错误。
    fn new() -> windows_core::Result<Self> {
        unsafe {
            let factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |font, size, align| -> windows_core::Result<IDWriteTextFormat> {
                let f = write.CreateTextFormat(
                    font,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("zh-CN"),
                )?;
                f.SetTextAlignment(align).ok()?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                    .ok()?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
                Ok(f)
            };
            Ok(Self {
                factory,
                text: format(w!("SimSun"), 16.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                label: format(w!("SimSun"), 12.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                mode_indicator: format(w!("SimSun"), 16.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                write,
                target: None,
            })
        }
    }
    /// 首次使用或目标已丢弃时创建渲染目标，已有目标保持不变。
    ///
    /// 尺寸取 HWND 当前客户区且每边至少为一个像素；创建失败时不留下半成品。
    fn ensure_target(&mut self, hwnd: HWND, dpi: u32) -> windows_core::Result<()> {
        if self.target.is_none() {
            unsafe {
                let mut rc = RECT::default();
                GetClientRect(hwnd, &mut rc).ok()?;
                let properties = D2D1_RENDER_TARGET_PROPERTIES {
                    dpiX: dpi as f32,
                    dpiY: dpi as f32,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                self.target = Some(self.factory.CreateHwndRenderTarget(
                    &properties,
                    &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd,
                        pixelSize: D2D_SIZE_U {
                            width: (rc.right - rc.left).max(1) as u32,
                            height: (rc.bottom - rc.top).max(1) as u32,
                        },
                        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                    },
                )?);
            }
        }
        Ok(())
    }
}

/// 某一可见窗口当前呈现的数据快照、事件接收端和命中区域。
struct Content {
    /// 用于绘制和判断控件是否仍可操作的最新快照。
    snapshot: CandidateView,
    /// 点击释放时接收 UI 操作的发送端。
    events: EventSink,
    /// 按当前窗口角色和候选数量计算的 DIP 布局。
    layout: Layout,
}
/// 窗口独占的可变 UI 状态；借用期间不得调用可能同步重入窗口过程的原生 API。
struct App {
    /// 当前窗口的图形资源。
    graphics: Graphics,
    /// 无内容时为 `None`，避免隐藏窗口继续持有旧快照和事件发送端。
    content: Option<Content>,
    /// 鼠标按压与悬停状态机。
    gesture: Gesture,
}

/// 窗口过程回调可访问的稳定状态分配。
///
/// 此分配必须至少存活到 HWND 销毁；任何可能同步分派消息的原生调用之前，
/// 都不能持有指向本对象或其 [`App`] 的可变借用，以免重入导致 RefCell 冲突。
struct Window {
    /// 是否启用文本抗锯齿。
    antialiasing: bool,
    /// 原生 HWND；销毁通知到达后清空。
    hwnd: Cell<HWND>,
    /// 当前窗口 DPI，始终至少为 1。
    dpi: Cell<u32>,
    /// 渲染与交互状态。只在 UI 线程访问，不跨线程共享。
    app: RefCell<App>,
    /// 首个致命错误；设置后由健康检查持续报告。
    error: RefCell<Option<String>>,
    /// Direct2D 设备丢失的有限重试状态。
    recovery: RefCell<Recovery>,
    /// 防止定位期间由 DPI 消息引发递归定位。
    positioning: Cell<bool>,
    /// 此窗口是否属于可交互的主题预览模式。
    preview: bool,
    /// 输入框、候选窗或模式提示窗，决定布局及交互策略。
    role: Role,
}

/// 主题后端持有三个 HWND 状态分配，并与原生回调共享其稳定地址。
struct Abc {
    /// 输入框以候选窗为 owner；声明顺序确保输入框先于候选窗销毁。
    input: Rc<Window>,
    /// 独立正方形模式提示窗；不依附会在提示期间隐藏的候选窗。
    mode_indicator: Rc<Window>,
    /// 候选窗及其对应的状态。
    candidates: Rc<Window>,
}

/// 创建候选窗及其 owner 输入窗和模式提示窗；任一窗口创建失败都会返回错误。
fn create(mode: UiMode, antialiasing: bool) -> Result<Box<dyn ThemeBackend>, String> {
    let candidates = create_window(mode, Role::Candidates, None, antialiasing)?;
    let input = create_window(mode, Role::Input, Some(candidates.hwnd.get()), antialiasing)?;
    let mode_indicator = create_window(mode, Role::ModeIndicator, None, antialiasing)?;
    Ok(Box::new(Abc {
        input,
        mode_indicator,
        candidates,
    }))
}

/// 创建一个窗口及其状态，并在返回前验证渲染目标和窗口健康状态。
///
/// `owner` 仅用于建立原生窗口所有权关系；`mode` 决定是否使用预览窗口策略。
fn create_window(
    mode: UiMode,
    role: Role,
    owner: Option<HWND>,
    antialiasing: bool,
) -> Result<Rc<Window>, String> {
    let preview = mode != UiMode::Live;
    let window = Rc::new(Window {
        antialiasing,
        hwnd: Cell::new(HWND::default()),
        dpi: Cell::new(96),
        app: RefCell::new(App {
            graphics: Graphics::new().map_err(|e| e.to_string())?,
            content: None,
            gesture: Gesture::default(),
        }),
        error: RefCell::new(None),
        recovery: RefCell::new(Recovery::default()),
        positioning: Cell::new(false),
        preview,
        role,
    });
    unsafe {
        let instance = GetModuleHandleW(None);
        if instance.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        // The class icon is the task-bar button icon for the preview window. The
        // live tool window has no task-bar presence, so it is harmless there too.
        let icon = LoadIconW(Some(instance), w!("WEASEL_ICON"));
        let mut existing = WNDCLASSW::default();
        if !GetClassInfoW(Some(instance), CLASS, &mut existing).as_bool() {
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(wnd_proc),
                hCursor: LoadCursorW(None, IDC_ARROW),
                // Class icon also acts as the preview task-bar icon fallback.
                hIcon: icon,
                ..Default::default()
            };
            if wc.hCursor.0.is_null() || RegisterClassW(&wc).0 == 0 {
                return Err(windows_core::Error::from_thread().to_string());
            }
        }
        // The preview keeps the borderless strip identical to input but must be
        // findable and closable: no WS_EX_TOOLWINDOW (task-bar button) and no
        // WS_EX_NOACTIVATE (activatable, so it can be closed).
        let (ex_style, title) = if preview && role == Role::Candidates {
            ((WS_EX_TOPMOST) as u32, w!("Weasel-RS 皮肤预览"))
        } else {
            (
                (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as u32,
                w!("Weasel candidates"),
            )
        };
        let hwnd = CreateWindowExW(
            ex_style,
            CLASS,
            title,
            WS_POPUP | if preview { WS_SYSMENU as u32 } else { 0 },
            0,
            0,
            1,
            1,
            owner,
            None,
            Some(instance),
            Some(Rc::as_ptr(&window).cast()),
        );
        if hwnd.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        window.hwnd.set(hwnd);
        window.dpi.set(GetDpiForWindow(hwnd).max(1));
        if preview {
            apply_taskbar_icon(hwnd, icon);
        }
        // Eager initialization means the factory can fail over before first show.
        window
            .app
            .borrow_mut()
            .graphics
            .ensure_target(hwnd, window.dpi.get())
            .map_err(|e| e.to_string())?;
    }
    window.health()?;
    Ok(window)
}

/// 设置预览窗口的小图标和大图标，使任务栏按钮显示 Weasel 图标。
///
/// 空图标不执行任何操作；`WM_SETICON` 的发送结果不作为创建失败处理。
unsafe fn apply_taskbar_icon(hwnd: HWND, icon: HICON) {
    if icon.0.is_null() {
        return;
    }
    unsafe {
        let _ = SendMessageW(
            hwnd,
            WM_SETICON as u32,
            WPARAM(ICON_SMALL as usize),
            LPARAM(icon.0 as isize),
        );
        let _ = SendMessageW(
            hwnd,
            WM_SETICON as u32,
            WPARAM(ICON_BIG as usize),
            LPARAM(icon.0 as isize),
        );
    }
}

impl Window {
    /// 记录首个错误，避免后续连锁错误覆盖最初的故障原因。
    fn fail(&self, error: impl ToString) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error.to_string());
        }
    }
    /// 请求系统重绘当前 HWND；调用失败不改变窗口状态。
    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }
    /// 清除手势并在本窗口拥有鼠标捕获时释放捕获。
    fn cancel(&self) {
        self.app.borrow_mut().gesture.cancel();
        unsafe {
            if GetCapture() == self.hwnd.get() {
                let _ = ReleaseCapture();
            }
        }
    }
    /// 根据快照、窗口角色和 DPI 计算位置并移动窗口。
    ///
    /// 无可定位内容时成功返回；重入请求直接忽略。移动可能同步触发 DPI 变化，
    /// 因而最多重新计算三次，原生定位错误向调用方传播。
    fn position(&self) -> windows_core::Result<()> {
        if self.positioning.replace(true) {
            return Ok(());
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.positioning);
        // Moving between monitors may synchronously change DPI. Recompute once
        // that call returns, with a finite bound for pathological monitor layouts.
        for _ in 0..3 {
            let dpi = self.dpi.get();
            let bounds = {
                let app = self.app.borrow();
                app.content.as_ref().and_then(|c| {
                    let candidate = Layout::new(c.snapshot.items.len(), Role::Candidates);
                    if self.role == Role::ModeIndicator {
                        let width = pixels(c.layout.width, dpi);
                        let height = pixels(c.layout.height, dpi);
                        let (x, y) = if self.preview {
                            crate::presentation::preview_position(width, height)
                        } else {
                            let anchor = c.snapshot.anchor.as_ref().filter(|a| a.valid)?;
                            crate::presentation::popup_position(anchor, width, height)
                        };
                        return Some((x, y, width, height));
                    }
                    let input = c.snapshot.preedit.is_some();
                    let candidates = !c.snapshot.items.is_empty();
                    let candidate_offset = if input && candidates {
                        logic::INPUT_WIDTH + logic::GAP
                    } else {
                        0.0
                    };
                    let group_width = if candidates {
                        candidate_offset + candidate.width
                    } else {
                        logic::INPUT_WIDTH
                    };
                    let group_height = if candidates {
                        candidate.height
                    } else {
                        logic::INPUT_HEIGHT
                    };
                    if self.preview {
                        let (x, y) = crate::presentation::preview_position(
                            pixels(group_width, dpi),
                            pixels(group_height, dpi),
                        );
                        return Some((
                            x + if self.role == Role::Candidates && input {
                                pixels(candidate_offset, dpi)
                            } else {
                                0
                            },
                            y,
                            pixels(c.layout.width, dpi),
                            pixels(c.layout.height, dpi),
                        ));
                    }
                    let anchor = c.snapshot.anchor.as_ref().filter(|a| a.valid)?;
                    let (x, y) = if input {
                        let input_position = crate::presentation::popup_position(
                            anchor,
                            pixels(logic::INPUT_WIDTH, dpi),
                            pixels(logic::INPUT_HEIGHT, dpi),
                        );
                        if self.role == Role::Candidates {
                            if let Some(work) = crate::presentation::work_area(anchor) {
                                logic::candidate_position(
                                    input_position,
                                    pixels(logic::INPUT_WIDTH, dpi),
                                    (pixels(candidate.width, dpi), pixels(candidate.height, dpi)),
                                    pixels(logic::GAP, dpi),
                                    &work,
                                )
                            } else {
                                (
                                    input_position.0 + pixels(candidate_offset, dpi),
                                    input_position.1,
                                )
                            }
                        } else {
                            input_position
                        }
                    } else {
                        crate::presentation::popup_position(
                            anchor,
                            pixels(candidate.width, dpi),
                            pixels(candidate.height, dpi),
                        )
                    };
                    Some((
                        x,
                        y,
                        pixels(c.layout.width, dpi),
                        pixels(c.layout.height, dpi),
                    ))
                })
            };
            let Some((x, y, width, height)) = bounds else {
                return Ok(());
            };
            unsafe {
                SetWindowPos(
                    self.hwnd.get(),
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE as u32,
                )
                .ok()?;
            }
            if dpi == self.dpi.get() {
                break;
            }
        }
        Ok(())
    }
    /// 将当前客户区尺寸和 DPI 同步到已创建的渲染目标。
    fn resize(&self) -> windows_core::Result<()> {
        let app = self.app.borrow();
        if let Some(target) = &app.graphics.target {
            unsafe {
                let mut rc = RECT::default();
                GetClientRect(self.hwnd.get(), &mut rc).ok()?;
                target.SetDpi(self.dpi.get() as f32, self.dpi.get() as f32);
                target
                    .Resize(&D2D_SIZE_U {
                        width: (rc.right - rc.left).max(1) as u32,
                        height: (rc.bottom - rc.top).max(1) as u32,
                    })
                    .ok()?;
            }
        }
        Ok(())
    }
    /// 丢弃可能失效的渲染目标，并按恢复策略安排重试或记录致命错误。
    ///
    /// 只有设备丢失且重试次数未耗尽时才设置短定时器；定时器创建失败会转为
    /// 健康错误，后续绘制不再继续。
    fn graphics_error(&self, error: windows_core::Error) {
        self.app.borrow_mut().graphics.target = None;
        if !self
            .recovery
            .borrow_mut()
            .failed(error.code() == D2DERR_RECREATE_TARGET)
        {
            self.fail(error);
            return;
        }
        unsafe {
            if SetTimer(Some(self.hwnd.get()), RETRY_TIMER, 100, None) == 0 {
                self.fail(windows_core::Error::from_thread());
            }
        }
    }
    /// 绘制当前内容；绘制结束后才将恢复计数清零。
    ///
    /// Direct2D/DirectWrite 错误由调用方交给 [`Window::graphics_error`] 处理。
    fn paint(&self) -> windows_core::Result<()> {
        let mut app = self.app.borrow_mut();
        app.graphics
            .ensure_target(self.hwnd.get(), self.dpi.get())?;
        let target = app
            .graphics
            .target
            .as_ref()
            .ok_or_else(|| windows_core::Error::from_hresult(E_UNEXPECTED))?;
        unsafe {
            let brush = target.CreateSolidColorBrush(&color(0x000000), None)?;
            target.SetTextAntialiasMode(if self.antialiasing {
                D2D1_TEXT_ANTIALIAS_MODE_DEFAULT
            } else {
                D2D1_TEXT_ANTIALIAS_MODE_ALIASED
            });
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            target.BeginDraw();
            let draw = DrawGuard {
                target,
                ended: false,
            };
            target.Clear(Some(&color(0xC0C0C0)));
            if let Some(c) = &app.content {
                let width = c.layout.width;
                let height = c.layout.height;
                draw_frame(target, &brush, width, height);
                if self.role == Role::Input
                    && let Some(preedit) = &c.snapshot.preedit
                {
                    let area = rect(PAD, PAD, width - 6.0, height - PAD);
                    let chars: Vec<u16> = preedit.text.encode_utf16().collect();
                    let layout = app.graphics.write.CreateTextLayout(
                        &chars,
                        &app.graphics.text,
                        1_000_000.0,
                        height - 2.0 * PAD,
                    )?;
                    let mut x = 0.0;
                    let mut y = 0.0;
                    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                    layout
                        .HitTestTextPosition(preedit.cursor, false, &mut x, &mut y, &mut metrics)
                        .ok()?;
                    let scroll = (x - (area.right - area.left - 3.0)).max(0.0);
                    target.PushAxisAlignedClip(&area, D2D1_ANTIALIAS_MODE_ALIASED);
                    brush.SetColor(&color(0x000000));
                    target.DrawTextLayout(
                        windows_numerics::Vector2 {
                            x: area.left - scroll,
                            y: area.top,
                        },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                    brush.SetColor(&color(0x3FC0C0));
                    let scale = self.dpi.get() as f32 / 96.0;
                    let caret_x = ((area.left + x - scroll) * scale).round() / scale;
                    let caret_width = (2.0 * scale).round().max(1.0) / scale;
                    target.FillRectangle(
                        &rect(
                            caret_x,
                            ((area.top + 1.0) * scale).round() / scale,
                            caret_x + caret_width,
                            ((area.bottom - 1.0) * scale).round() / scale,
                        ),
                        &brush,
                    );
                    target.PopAxisAlignedClip();
                }
                if self.role == Role::Candidates {
                    text(
                        target,
                        &brush,
                        &app.graphics.label,
                        "数字",
                        rect(
                            (width - 32.0) / 2.0,
                            height - 18.0,
                            (width + 32.0) / 2.0,
                            height - 4.0,
                        ),
                        0x000080,
                    );
                }
                if self.role == Role::ModeIndicator
                    && let Some(indicator) = &c.snapshot.mode_indicator
                {
                    text(
                        target,
                        &brush,
                        &app.graphics.mode_indicator,
                        if indicator.ascii_mode { "英" } else { "中" },
                        rect(PAD, PAD, width - PAD, height - PAD),
                        0x800080,
                    );
                }
                for cell in &c.layout.cells {
                    let usable = enabled(&c.snapshot, cell.hit);

                    let pressed = app.gesture.pressed == Some(cell.hit);
                    let bounds = rect(cell.left, cell.top, cell.right, cell.bottom);
                    if pressed {
                        brush.SetColor(&color(0xA0A0A0));
                        target.FillRectangle(&bounds, &brush);
                    }
                    let (label, rgb) = match cell.hit {
                        Hit::Candidate(i) => {
                            let item = &c.snapshot.items[i];
                            (
                                format!("{}:{}", i + 1, item.primary_text),
                                if !usable { 0x808080 } else { 0x800080 },
                            )
                        }
                        Hit::Previous
                        | Hit::Next
                        | Hit::PreviousDecorative
                        | Hit::NextDecorative => {
                            draw_arrow(
                                target,
                                &brush,
                                cell.left,
                                cell.top,
                                matches!(cell.hit, Hit::Previous | Hit::PreviousDecorative),
                                usable,
                                pressed,
                            );
                            if matches!(cell.hit, Hit::PreviousDecorative | Hit::NextDecorative) {
                                let y = cell.top
                                    + if cell.hit == Hit::PreviousDecorative {
                                        3.0
                                    } else {
                                        10.0
                                    };
                                let offset = if pressed { 1.0 } else { 0.0 };
                                brush.SetColor(&color(if usable { 0x000000 } else { 0x808080 }));
                                target.FillRectangle(
                                    &rect(
                                        cell.left + 3.0 + offset,
                                        y + offset,
                                        cell.left + 10.0 + offset,
                                        y + 1.0 + offset,
                                    ),
                                    &brush,
                                );
                            }
                            continue;
                        }
                    };
                    text(
                        target,
                        &brush,
                        &app.graphics.text,
                        &label,
                        rect(cell.left, cell.top, cell.right, cell.bottom),
                        rgb,
                    );
                }
            }
            draw.finish()?;
        }
        self.recovery.borrow_mut().succeeded();
        Ok(())
    }
    /// 将客户区像素坐标转换为 DIP，并在当前布局中查找命中控件。
    fn hit(&self, param: LPARAM) -> Option<Hit> {
        let x = param.0 as u16 as i16 as f32 * 96.0 / self.dpi.get() as f32;
        let y = (param.0 >> 16) as u16 as i16 as f32 * 96.0 / self.dpi.get() as f32;
        self.app
            .borrow()
            .content
            .as_ref()
            .and_then(|c| c.layout.hit(x, y))
    }
}

impl ThemeBackend for Abc {
    /// 在普通候选界面与独立模式提示窗之间切换；任一侧失败时隐藏整组界面。
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        let show_indicator = crate::presentation::is_mode_indicator_visible(snapshot)
            && !crate::presentation::is_visible(snapshot);
        let result = if show_indicator {
            // Hide the larger windows before exposing the transient square so a
            // transition cannot briefly show both presentations.
            self.input.hide();
            self.candidates.hide();
            self.mode_indicator.render(snapshot, events)
        } else {
            self.mode_indicator.hide();
            self.candidates
                .render(snapshot, events)
                .and_then(|_| self.input.render(snapshot, events))
        };
        if result.is_err() {
            self.hide();
        }
        result
    }
    /// 隐藏三个窗口，并清除各自的交互与快照状态。
    fn hide(&mut self) {
        self.input.hide();
        self.mode_indicator.hide();
        self.candidates.hide();
    }
    /// 请求三个窗口重绘并检查其健康状态。
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.input.invalidate();
        self.mode_indicator.invalidate();
        self.candidates.invalidate();
        self.check_health()
    }
    /// 按输入窗、模式提示窗、候选窗顺序返回首个已记录的致命错误。
    fn check_health(&mut self) -> Result<(), String> {
        self.input
            .health()
            .and_then(|_| self.mode_indicator.health())
            .and_then(|_| self.candidates.health())
    }
}

impl Window {
    /// 应用新快照，并按可见性决定只更新定位还是重建并显示内容。
    ///
    /// 预编辑游标等仅布局变化可复用当前内容，不取消按压手势，也不重建布局；
    /// 内容或可见性发生变化时先取消手势。快照校验、定位和健康检查错误均向上传播。
    fn render(&self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        if let Some(preedit) = &snapshot.preedit {
            preedit.validate()?;
        }
        // Layout-only snapshots must not cancel a pressed candidate, rebuild
        // text layouts, or repaint content. position() handles DPI transitions.
        let moved = {
            let mut app = self.app.borrow_mut();
            if let Some(content) = app.content.as_mut()
                && self.is_visible(snapshot)
                && crate::theme_api::same_content(&content.snapshot, snapshot)
            {
                content.snapshot = snapshot.clone();
                content.events = events.clone();
                true
            } else {
                false
            }
        };
        if moved {
            self.position().map_err(|e| e.to_string())?;
            return self.health();
        }
        self.cancel();
        self.health()?;
        if !self.is_visible(snapshot) {
            self.hide();
            return Ok(());
        }
        let was_hidden = self.app.borrow().content.is_none();
        {
            let mut app = self.app.borrow_mut();
            app.content = Some(Content {
                snapshot: snapshot.clone(),
                events: events.clone(),
                layout: Layout::new(snapshot.items.len(), self.role),
            });
        }
        self.position().map_err(|e| e.to_string())?;
        if was_hidden {
            // A hidden HWND render target retains its last presented pixels.
            // Replace them before the window can expose the previous composition.
            if let Err(error) = self.paint() {
                self.graphics_error(error);
            }
        }
        unsafe {
            let _ = ShowWindow(
                self.hwnd.get(),
                if self.preview && self.role == Role::Candidates {
                    SW_SHOW
                } else {
                    SW_SHOWNOACTIVATE
                },
            );
        }
        self.invalidate();
        self.health()
    }
    /// 判断当前角色是否应消费并展示这份快照。
    fn is_visible(&self, snapshot: &CandidateView) -> bool {
        match self.role {
            Role::Input => crate::presentation::is_visible(snapshot) && snapshot.preedit.is_some(),
            Role::Candidates => {
                crate::presentation::is_visible(snapshot) && !snapshot.items.is_empty()
            }
            Role::ModeIndicator => {
                crate::presentation::is_mode_indicator_visible(snapshot)
                    && !crate::presentation::is_visible(snapshot)
            }
        }
    }
    /// 清除当前内容、停止恢复定时器并隐藏 HWND。
    fn hide(&self) {
        self.cancel();
        self.app.borrow_mut().content = None;
        self.recovery.borrow_mut().waiting = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd.get()), RETRY_TIMER);
            let _ = ShowWindow(self.hwnd.get(), SW_HIDE);
        }
    }
    /// 返回创建或回调过程中记录的首个错误。
    fn health(&self) -> Result<(), String> {
        self.error
            .borrow()
            .as_ref()
            .map_or(Ok(()), |e| Err(e.clone()))
    }
}

impl Drop for Window {
    /// 在释放状态前解除回调指针，再停止定时器并销毁 HWND。
    fn drop(&mut self) {
        let hwnd = self.hwnd.get();
        if !hwnd.0.is_null() {
            unsafe {
                // Detach before destruction: no callback can access a dropping App.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                self.cancel();
                let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

/// Win32 绘制周期的 RAII 守卫，确保每次成功开始的绘制都会结束。
struct PaintGuard {
    /// 本次绘制所属窗口。
    hwnd: HWND,
    /// `BeginPaint` 填充并由 `EndPaint` 配对使用的状态。
    ps: PAINTSTRUCT,
}
impl PaintGuard {
    /// 开始一次 Win32 绘制周期；析构时配对调用 `EndPaint`。
    unsafe fn begin(hwnd: HWND) -> Self {
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let _ = BeginPaint(hwnd, &mut ps);
        }
        Self { hwnd, ps }
    }
}
impl Drop for PaintGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = EndPaint(self.hwnd, &self.ps);
        }
    }
}
/// Direct2D 绘制周期的 RAII 守卫；提前返回时尽力结束绘制。
struct DrawGuard<'a> {
    /// 此守卫借用的渲染目标，保证其存活至绘制周期结束。
    target: &'a ID2D1HwndRenderTarget,
    /// 显式 `finish` 后置位，避免析构时重复调用 `EndDraw`。
    ended: bool,
}
impl DrawGuard<'_> {
    /// 显式结束 Direct2D 绘制并返回其设备错误。
    fn finish(mut self) -> windows_core::Result<()> {
        self.ended = true;
        unsafe { self.target.EndDraw(None, None).ok() }
    }
}
impl Drop for DrawGuard<'_> {
    fn drop(&mut self) {
        if !self.ended {
            unsafe {
                let _ = self.target.EndDraw(None, None);
            }
        }
    }
}

/// Win32 窗口过程入口：在跨越系统回调 ABI 边界前捕获所有 Rust panic，避免异常展开进入系统代码。
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| unsafe { dispatch(hwnd, msg, wp, lp) })) {
        Ok(result) => result,
        Err(_) => {
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Window;
                if let Some(window) = ptr.as_ref() {
                    if let Ok(mut error) = window.error.try_borrow_mut() {
                        *error = Some("theme_abc native callback panicked".into());
                    }
                }
            }
            LRESULT(0)
        }
    }
}

/// 分派单个 Win32 消息；仅在这里访问绑定到 HWND 的 [`Window`] 指针。
///
/// `WM_NCCREATE` 安装由后端持有的稳定指针，`WM_NCDESTROY` 将其解除；输入事件
/// 只有在按下与释放命中同一且仍启用的控件时才发送给主题事件接收端。
unsafe fn dispatch(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE as u32 {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            let ptr = create.lpCreateParams as *const Window;
            if ptr.is_null() {
                return LRESULT(0);
            }
            (*ptr).hwnd.set(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            return LRESULT(1);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Window;
        if let Some(window) = ptr.as_ref() {
            match msg as i32 {
                WM_PAINT => {
                    let _paint = PaintGuard::begin(hwnd);
                    let can_paint =
                        window.error.borrow().is_none() && !window.recovery.borrow().waiting;
                    if can_paint {
                        if let Err(e) = window.paint() {
                            window.graphics_error(e);
                        }
                    }
                    return LRESULT(0);
                }
                WM_ERASEBKGND => return LRESULT(1),
                WM_CLOSE => {
                    // Closing the preview window ends the process. The live tool
                    // window is destroyed by the shutdown path and never posts
                    // WM_CLOSE.
                    if window.preview {
                        PostQuitMessage(0);
                        // Leave HWND destruction to the owning backend's Drop.
                        return LRESULT(0);
                    }
                    return DefWindowProcW(hwnd, msg, wp, lp);
                }
                WM_MOUSEACTIVATE => {
                    // The preview window must be activatable so it can be closed
                    // (Alt+F4); the live strip never activates the host.
                    if window.preview && window.role == Role::Candidates {
                        return DefWindowProcW(hwnd, msg, wp, lp);
                    }
                    return LRESULT(MA_NOACTIVATE as isize);
                }
                WM_SIZE => {
                    if let Err(e) = window.resize() {
                        window.graphics_error(e);
                    }
                    return LRESULT(0);
                }
                WM_DPICHANGED => {
                    window.dpi.set((wp.0 as u32 & 0xffff).max(1));
                    window.cancel();
                    if let Err(e) = window.position() {
                        window.fail(e);
                    }
                    if let Err(e) = window.resize() {
                        window.graphics_error(e);
                    }
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_TIMER if wp.0 == RETRY_TIMER => {
                    let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                    window.recovery.borrow_mut().waiting = false;
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN => {
                    let hit = window.hit(lp);
                    let pressed = {
                        let mut app = window.app.borrow_mut();
                        let App {
                            content, gesture, ..
                        } = &mut *app;
                        if let Some(c) = content {
                            gesture.press(hit, &c.snapshot);
                        }
                        gesture.pressed.is_some()
                    };
                    if pressed {
                        let _ = SetCapture(hwnd);
                    }
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_LBUTTONUP => {
                    let hit = window.hit(lp);
                    let event = {
                        let mut app = window.app.borrow_mut();
                        let App {
                            content, gesture, ..
                        } = &mut *app;
                        content.as_ref().and_then(|c| {
                            gesture
                                .release(hit, &c.snapshot)
                                .map(|e| (c.events.clone(), e))
                        })
                    };
                    window.cancel();
                    if let Some((sender, event)) = event {
                        sender.send(event);
                    }
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_MOUSEMOVE => {
                    let hit = window.hit(lp);
                    window.app.borrow_mut().gesture.motion(hit);
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE as u32,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_MOUSELEAVE | WM_CAPTURECHANGED | WM_CANCELMODE => {
                    window.cancel();
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    window.hwnd.set(HWND::default());
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

fn color(rgb: u32) -> D2D_COLOR_F {
    D2D_COLOR_F {
        r: ((rgb >> 16) & 255) as f32 / 255.0,
        g: ((rgb >> 8) & 255) as f32 / 255.0,
        b: (rgb & 255) as f32 / 255.0,
        a: 1.0,
    }
}
/// 构造指定边界的 Direct2D 矩形（坐标单位为 DIP）。
fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}
/// 绘制单行 UTF-16 文本；临时编码缓冲区只在本次 Direct2D 调用期间存在。
unsafe fn text(
    target: &ID2D1HwndRenderTarget,
    brush: &ID2D1SolidColorBrush,
    format: &IDWriteTextFormat,
    value: &str,
    rect: D2D_RECT_F,
    rgb: u32,
) {
    let value: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        brush.SetColor(&color(rgb));
        target.DrawText(
            &value,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// ABC 主题工厂；默认配置来自同目录的 `config.json`。
pub struct Factory;

impl crate::theme_api::ThemeFactory for Factory {
    /// 返回注册表使用的主题标识。
    fn name(&self) -> &'static str {
        "abc"
    }
    /// 声明支持预编辑框且不要求常驻窗口。
    fn capabilities(&self) -> crate::theme_api::ThemeCapabilities {
        crate::theme_api::ThemeCapabilities {
            preedit: true,
            resident: false,
            mode_indicator: true,
        }
    }
    /// 解析编译期嵌入的默认配置；配置无效时返回带主题上下文的错误。
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(include_str!("config.json"))
            .map_err(|error| format!("invalid ABC default settings: {error}"))
    }
    /// 从配置快照读取抗锯齿选项并创建后端；缺项、类型错误或窗口初始化失败都会体现在创建结果中。
    fn create(
        &self,
        mode: UiMode,
        settings: &weasel_common::settings::ConfigSnapshot,
    ) -> crate::theme_api::ThemeCreation {
        (|| -> Result<Box<dyn ThemeBackend>, String> {
            let antialiasing = settings
                .get::<bool>(".themeSettings.abc.antialiasing")?
                .ok_or("missing ABC antialiasing setting")?;
            create(mode, antialiasing)
        })()
        .into()
    }
}

/// 绘制三层经典凸起边框；边线颜色顺序构成外、中、内三道明暗反差。
unsafe fn draw_frame(target: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, w: f32, h: f32) {
    unsafe {
        for (inset, light, dark) in [
            (0.0, 0xC0C0C0, 0x000000),
            (1.0, 0xFFFFFF, 0x808080),
            (3.0, 0x808080, 0xFFFFFF),
        ] {
            for (edge, rgb) in [
                (rect(inset, inset, w - inset - 1.0, inset + 1.0), light),
                (rect(inset, inset, inset + 1.0, h - inset - 1.0), light),
                (rect(inset, h - inset - 1.0, w - inset, h - inset), dark),
                (rect(w - inset - 1.0, inset, w - inset, h - inset), dark),
            ] {
                brush.SetColor(&color(rgb));
                target.FillRectangle(&edge, brush);
            }
        }
    }
}
/// 绘制带按下态和禁用态反馈的分页箭头按钮。
///
/// `up` 控制箭头方向，`enabled` 只影响箭头颜色，`pressed` 同时改变按钮边框和图形偏移。
unsafe fn draw_arrow(
    target: &ID2D1HwndRenderTarget,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    y: f32,
    up: bool,
    enabled: bool,
    pressed: bool,
) {
    unsafe {
        // Separate button identities preserve cancellation when moving between
        // the two controls which happen to invoke the same page action.
        for (edge, rgb) in [
            (
                rect(x, y, x + 13.0, y + 1.0),
                if pressed { 0x404040 } else { 0xFFFFFF },
            ),
            (
                rect(x, y, x + 1.0, y + 13.0),
                if pressed { 0x404040 } else { 0xFFFFFF },
            ),
            (
                rect(x, y + 13.0, x + 14.0, y + 14.0),
                if pressed { 0xFFFFFF } else { 0x404040 },
            ),
            (
                rect(x + 13.0, y, x + 14.0, y + 14.0),
                if pressed { 0xFFFFFF } else { 0x404040 },
            ),
        ] {
            brush.SetColor(&color(rgb));
            target.FillRectangle(&edge, brush);
        }
        brush.SetColor(&color(if enabled { 0x000000 } else { 0x808080 }));
        let offset = if pressed { 1.0 } else { 0.0 };
        for row in 0..4 {
            let span = if up { row } else { 3 - row } as f32;
            target.FillRectangle(
                &rect(
                    x + 6.0 - span + offset,
                    y + 5.0 + row as f32 + offset,
                    x + 7.0 + span + offset,
                    y + 6.0 + row as f32 + offset,
                ),
                brush,
            );
        }
    }
}
