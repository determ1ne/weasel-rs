//! Windows 10 candidate strip. All HWND and graphics types are private to this backend.
mod bindings;
mod logic;

use crate::{backend::ThemeBackend, ui_runtime::EventSender};
use bindings::*;
use logic::{Gesture, HEIGHT, Hit, Layout, NUMBER, Palette, Recovery, SCALE, enabled, pixels};
use std::{
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};
use weasel_common::message::RenderSnapshot;
use windows_strings::w;

const CLASS: windows_strings::PCWSTR = w!("Weasel.ThemeTen.D2D");
const RETRY_TIMER: usize = 1;

struct Graphics {
    factory: ID2D1Factory,
    write: IDWriteFactory,
    number: IDWriteTextFormat,
    text: IDWriteTextFormat,
    comment: IDWriteTextFormat,
    icon: IDWriteTextFormat,
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
                    size * SCALE,
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
                number: format(w!("Segoe UI"), 27.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                text: format(
                    w!("Microsoft YaHei UI"),
                    27.0,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?,
                comment: format(
                    w!("Microsoft YaHei UI"),
                    20.0,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?,
                icon: format(w!("Segoe MDL2 Assets"), 20.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                write,
                target: None,
            })
        }
    }
    fn measure(&self, text: &str, format: &IDWriteTextFormat) -> windows_core::Result<f32> {
        unsafe {
            let text: Vec<u16> = text.encode_utf16().collect();
            let layout = self
                .write
                .CreateTextLayout(&text, format, 1_000_000.0, HEIGHT)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).ok()?;
            Ok(metrics.widthIncludingTrailingWhitespace)
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
    snapshot: RenderSnapshot,
    events: EventSender,
    layout: Layout,
    primary_widths: Vec<f32>,
}
struct App {
    graphics: Graphics,
    content: Option<Content>,
    gesture: Gesture,
    palette: Palette,
}

/// The stable allocation outlives its HWND. Native calls that can synchronously
/// dispatch messages never hold a mutable reference to this object or its App.
struct Window {
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    app: RefCell<App>,
    error: RefCell<Option<String>>,
    recovery: RefCell<Recovery>,
    positioning: Cell<bool>,
}

// Keep the native callback's allocation behind a shared reference even while
// ThemeBackend is called through &mut self.
struct Ten {
    // Shared allocation keeps the HWND's pointer stable without creating an
    // exclusive reference to Window when the backend is moved or rendered.
    window: Rc<Window>,
}

pub fn create() -> Result<Box<dyn ThemeBackend>, String> {
    let window = Rc::new(Window {
        hwnd: Cell::new(HWND::default()),
        dpi: Cell::new(96),
        app: RefCell::new(App {
            graphics: Graphics::new().map_err(|e| e.to_string())?,
            content: None,
            gesture: Gesture::default(),
            palette: Palette::new(crate::appearance::is_dark()),
        }),
        error: RefCell::new(None),
        recovery: RefCell::new(Recovery::default()),
        positioning: Cell::new(false),
    });
    unsafe {
        let instance = GetModuleHandleW(None);
        if instance.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        let mut existing = WNDCLASSW::default();
        if !GetClassInfoW(Some(instance), CLASS, &mut existing).as_bool() {
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(wnd_proc),
                hCursor: LoadCursorW(None, IDC_ARROW),
                ..Default::default()
            };
            if wc.hCursor.0.is_null() || RegisterClassW(&wc).0 == 0 {
                return Err(windows_core::Error::from_thread().to_string());
            }
        }
        let hwnd = CreateWindowExW(
            (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as u32,
            CLASS,
            w!("Weasel candidates"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            Some(Rc::as_ptr(&window).cast()),
        );
        if hwnd.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        window.hwnd.set(hwnd);
        window.dpi.set(GetDpiForWindow(hwnd).max(1));
        // Eager initialization means the factory can fail over before first show.
        window
            .app
            .borrow_mut()
            .graphics
            .ensure_target(hwnd, window.dpi.get())
            .map_err(|e| e.to_string())?;
    }
    window.health()?;
    Ok(Box::new(Ten { window }))
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
                    c.snapshot
                        .anchor
                        .as_ref()
                        .filter(|a| a.valid)
                        .map(|anchor| {
                            let width = pixels(c.layout.width, dpi);
                            let height = pixels(HEIGHT, dpi);
                            let (x, y) = crate::presentation::popup_position(anchor, width, height);
                            (x, y, width, height)
                        })
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
        let p = app.palette;
        unsafe {
            // Allocate fallible resources before BeginDraw; EndDraw is RAII too.
            let brush = target.CreateSolidColorBrush(&color(p.text), None)?;
            target.BeginDraw();
            let draw = DrawGuard {
                target,
                ended: false,
            };
            target.Clear(Some(&color(p.background)));
            if let Some(c) = &app.content {
                for cell in &c.layout.cells {
                    let selected = matches!(cell.hit, Hit::Candidate(i) if i == c.snapshot.selected_index as usize);
                    let usable = enabled(&c.snapshot, cell.hit);
                    let rect = rect(cell.left, 0.0, cell.right, HEIGHT);
                    if selected || (usable && app.gesture.hovered == Some(cell.hit)) {
                        brush.SetColor(&color(if selected { p.active } else { p.hover }));
                        target.FillRectangle(&rect, &brush);
                    }
                    match cell.hit {
                        Hit::Candidate(i) => {
                            let item = &c.snapshot.items[i];
                            text(
                                target,
                                &brush,
                                &app.graphics.number,
                                &(i + 1).to_string(),
                                rect_with(rect, cell.left, cell.left + NUMBER - 8.0 * SCALE),
                                if !usable {
                                    p.disabled
                                } else if selected {
                                    p.active_number
                                } else {
                                    p.secondary
                                },
                            );
                            text(
                                target,
                                &brush,
                                &app.graphics.text,
                                &item.primary_text,
                                rect_with(rect, cell.left + NUMBER, cell.right - logic::PAD),
                                if usable { p.text } else { p.disabled },
                            );
                            if !item.secondary_text.is_empty() {
                                text(
                                    target,
                                    &brush,
                                    &app.graphics.comment,
                                    &item.secondary_text,
                                    rect_with(
                                        rect,
                                        cell.left + NUMBER + c.primary_widths[i] + 8.0 * SCALE,
                                        cell.right - logic::PAD,
                                    ),
                                    if usable { p.secondary } else { p.disabled },
                                );
                            }
                        }
                        hit => {
                            let glyph = match hit {
                                Hit::Previous => "\u{E76B}",
                                Hit::Next => "\u{E76C}",
                                _ => "\u{E76E}",
                            };
                            let mut icon_rect = rect;
                            icon_rect.top -= 2.0 * SCALE;
                            icon_rect.bottom -= 2.0 * SCALE;
                            text(
                                target,
                                &brush,
                                &app.graphics.icon,
                                glyph,
                                icon_rect,
                                if usable { p.text } else { p.disabled },
                            );
                        }
                    }
                }
                brush.SetColor(&color(p.border));
                for cell in &c.layout.cells {
                    if matches!(cell.hit, Hit::Previous | Hit::Emoji) {
                        target.FillRectangle(
                            &rect(cell.left, 0.0, cell.left + SCALE, HEIGHT),
                            &brush,
                        );
                    }
                }
                for edge in [
                    rect(0.0, 0.0, c.layout.width, SCALE),
                    rect(0.0, HEIGHT - SCALE, c.layout.width, HEIGHT),
                    rect(0.0, 0.0, SCALE, HEIGHT),
                    rect(c.layout.width - SCALE, 0.0, c.layout.width, HEIGHT),
                ] {
                    target.FillRectangle(&edge, &brush);
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

impl ThemeBackend for Ten {
    fn render(&mut self, snapshot: &RenderSnapshot, events: &EventSender) -> Result<(), String> {
        self.window.render(snapshot, events)
    }
    fn hide(&mut self) {
        self.window.hide();
    }
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.window.app.borrow_mut().palette = Palette::new(crate::appearance::is_dark());
        self.window.invalidate();
        self.window.health()
    }
    fn check_health(&mut self) -> Result<(), String> {
        self.window.health()
    }
}

impl Window {
    fn render(&self, snapshot: &RenderSnapshot, events: &EventSender) -> Result<(), String> {
        // Layout-only snapshots must not cancel a pressed candidate, rebuild
        // text layouts, or repaint content. position() handles DPI transitions.
        let moved = {
            let mut app = self.app.borrow_mut();
            if let Some(content) = app.content.as_mut()
                && crate::presentation::is_visible(snapshot)
                && crate::state::same_content(&content.snapshot, snapshot)
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
        if !snapshot.visible || !snapshot.anchor.as_ref().is_some_and(|a| a.valid) {
            self.hide();
            return Ok(());
        }
        {
            let mut app = self.app.borrow_mut();
            let mut primary_widths = Vec::with_capacity(snapshot.items.len());
            let mut widths = Vec::with_capacity(snapshot.items.len());
            for item in &snapshot.items {
                let primary = app
                    .graphics
                    .measure(&item.primary_text, &app.graphics.text)
                    .map_err(|e| e.to_string())?;
                let secondary = if item.secondary_text.is_empty() {
                    0.0
                } else {
                    8.0 * SCALE
                        + app
                            .graphics
                            .measure(&item.secondary_text, &app.graphics.comment)
                            .map_err(|e| e.to_string())?
                };
                primary_widths.push(primary);
                widths.push(primary + secondary);
            }
            app.content = Some(Content {
                snapshot: snapshot.clone(),
                events: events.clone(),
                layout: Layout::new(widths),
                primary_widths,
            });
        }
        self.position().map_err(|e| e.to_string())?;
        unsafe {
            let _ = ShowWindow(self.hwnd.get(), SW_SHOWNOACTIVATE);
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
                        *error = Some("theme_ten native callback panicked".into());
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
                WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
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
fn rect_with(mut rect: D2D_RECT_F, left: f32, right: f32) -> D2D_RECT_F {
    rect.left = left;
    rect.right = right;
    rect
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
