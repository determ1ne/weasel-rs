//! Classic ABC-inspired candidate window. No legacy code or resource assets are embedded.
//! Native lifetime/paint handling follows our D2D backend; input remains owned by TIP.
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

struct Graphics {
    factory: ID2D1Factory,
    write: IDWriteFactory,
    text: IDWriteTextFormat,
    label: IDWriteTextFormat,
    target: Option<ID2D1HwndRenderTarget>,
}

impl Graphics {
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
                write,
                target: None,
            })
        }
    }
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

struct Content {
    snapshot: CandidateView,
    events: EventSink,
    layout: Layout,
}
struct App {
    graphics: Graphics,
    content: Option<Content>,
    gesture: Gesture,
}

/// The stable allocation outlives its HWND. Native calls that can synchronously
/// dispatch messages never hold a mutable reference to this object or its App.
struct Window {
    antialiasing: bool,
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    app: RefCell<App>,
    error: RefCell<Option<String>>,
    recovery: RefCell<Recovery>,
    positioning: Cell<bool>,
    preview: bool,
    role: Role,
}

// Keep the native callback's allocation behind a shared reference even while
// ThemeBackend is called through &mut self.
struct Abc {
    // Input is owned by candidates and must be destroyed first.
    input: Rc<Window>,
    candidates: Rc<Window>,
}

fn create(mode: UiMode, antialiasing: bool) -> Result<Box<dyn ThemeBackend>, String> {
    let candidates = create_window(mode, Role::Candidates, None, antialiasing)?;
    let input = create_window(mode, Role::Input, Some(candidates.hwnd.get()), antialiasing)?;
    Ok(Box::new(Abc { input, candidates }))
}

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

/// Sets the window's small and big icons so the preview window's task-bar button
/// shows the Weasel icon. WM_SETICON is authoritative for the task-bar image.
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
    fn fail(&self, error: impl ToString) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error.to_string());
        }
    }
    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }
    fn cancel(&self) {
        self.app.borrow_mut().gesture.cancel();
        unsafe {
            if GetCapture() == self.hwnd.get() {
                let _ = ReleaseCapture();
            }
        }
    }
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
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        // Failure of either window hides the whole presentation.
        let result = self
            .candidates
            .render(snapshot, events)
            .and_then(|_| self.input.render(snapshot, events));
        if result.is_err() {
            self.hide();
        }
        result
    }
    fn hide(&mut self) {
        self.input.hide();
        self.candidates.hide();
    }
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.input.invalidate();
        self.candidates.invalidate();
        self.check_health()
    }
    fn check_health(&mut self) -> Result<(), String> {
        self.input.health().and_then(|_| self.candidates.health())
    }
}

impl Window {
    fn render(&self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        if let Some(preedit) = &snapshot.preedit {
            preedit.validate()?;
        }
        // Layout-only snapshots must not cancel a pressed candidate, rebuild
        // text layouts, or repaint content. position() handles DPI transitions.
        let moved = {
            let mut app = self.app.borrow_mut();
            if let Some(content) = app.content.as_mut()
                && crate::presentation::is_visible(snapshot)
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
        if !snapshot.visible
            || !snapshot.anchor.as_ref().is_some_and(|a| a.valid)
            || (self.role == Role::Input && snapshot.preedit.is_none())
            || (self.role == Role::Candidates && snapshot.items.is_empty())
        {
            self.hide();
            return Ok(());
        }
        {
            let mut app = self.app.borrow_mut();
            app.content = Some(Content {
                snapshot: snapshot.clone(),
                events: events.clone(),
                layout: Layout::new(snapshot.items.len(), self.role),
            });
        }
        self.position().map_err(|e| e.to_string())?;
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
    fn hide(&self) {
        self.cancel();
        self.app.borrow_mut().content = None;
        self.recovery.borrow_mut().waiting = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd.get()), RETRY_TIMER);
            let _ = ShowWindow(self.hwnd.get(), SW_HIDE);
        }
    }
    fn health(&self) -> Result<(), String> {
        self.error
            .borrow()
            .as_ref()
            .map_or(Ok(()), |e| Err(e.clone()))
    }
}

impl Drop for Window {
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

struct PaintGuard {
    hwnd: HWND,
    ps: PAINTSTRUCT,
}
impl PaintGuard {
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
struct DrawGuard<'a> {
    target: &'a ID2D1HwndRenderTarget,
    ended: bool,
}
impl DrawGuard<'_> {
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Catch every Rust callback, including paint and event delivery, before ABI return.
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
fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}
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

pub struct Factory;

impl crate::theme_api::ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        "abc"
    }
    fn capabilities(&self) -> crate::theme_api::ThemeCapabilities {
        crate::theme_api::ThemeCapabilities { preedit: true }
    }
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(include_str!("config.json"))
            .map_err(|error| format!("invalid ABC default settings: {error}"))
    }
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

// Classic bevel: outer gray/black, inner white/gray, inset gray/white.
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
