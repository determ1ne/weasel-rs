//! WASM 主题窗口：HWND 生命周期、DPI、锚点定位与消息分发。
//!
//! 原生资源全部留在创建它的 UI 线程上：
//! - HWND：类注册/创建/销毁、最顶层、live 模式不可激活
//! - DPI：`GetDpiForWindow` + `WM_DPICHANGED`
//! - 定位：锚点（物理像素）→ 工作区钳制 → `SetWindowPos`
//! - 动画帧：`SetTimer`(16ms) → wasm `frame(now_ms)`；主题不再请求时自然停止
//! - 设备丢失：`D2DERR_RECREATE_TARGET` → 有限重试（100ms，≤3 次，成功重置）
//!
//! 本模块不做布局：WASM 主题产出 [`crate::protocol::DrawCommand`] 命令流，
//! 窗口只在 `WM_PAINT` 用 canvas.rs 回放命令，并把鼠标/动画事件转发给
//! 运行时（`window.rs` + `canvas.rs` + `runtime.rs` 三层互不越界）。
//!
//! 借用语义：可能同步派发消息的原生调用（`SetWindowPos`）绝不持有
//! `App` 的可变借用（与 ten 主题相同的重入约束）。

use crate::appearance::{self};
use crate::d2d_bindings::*;
use crate::presentation::{is_visible, popup_position, preview_position};
use crate::protocol::{
    ACTION_DISMISS, ACTION_EMOJI, ACTION_ITEM, ACTION_NEXT, ACTION_PREVIOUS, DrawCommand,
    MOUSE_DOWN, MOUSE_LEAVE, MOUSE_MOVE, MOUSE_UP, PlacementStyle,
};
use crate::runtime::{WasmRuntime, now_ms};
use crate::theme_api::{
    Anchor, CandidateView, EventSink, NoticeSeverity, ThemeNotice, UiAction, same_content,
};
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use windows_strings::{PCWSTR, w};

const CLASS: PCWSTR = w!("Weasel.ThemeWasm.D2D");
const RETRY_TIMER: usize = 1;
const FRAME_TIMER: usize = 2;
const FRAME_INTERVAL_MS: u32 = 16;
const RETRY_INTERVAL_MS: u32 = 100;
/// notices 上限：防止主题刷日志导致无界增长（`take_notices` 会排空）。
const MAX_NOTICES: usize = 64;

/// 设备丢失重试状态（预算与 ten 主题一致：至多 3 次，成功重置）。
#[derive(Default)]
struct Recovery {
    failures: u8,
    waiting: bool,
}
impl Recovery {
    fn failed(&mut self, device_lost: bool) -> bool {
        if !device_lost || self.failures >= 3 {
            return false;
        }
        self.failures += 1;
        self.waiting = true;
        true
    }
    fn succeeded(&mut self) {
        self.failures = 0;
        self.waiting = false;
    }
}

#[derive(Clone, Copy)]
struct DragState {
    cursor_x: i32,
    cursor_y: i32,
    window_x: i32,
    window_y: i32,
}

/// DIP → 物理像素（96 基），最小 1px。
fn pixels(dip: f32, dpi: u32) -> i32 {
    (dip * dpi.max(1) as f32 / 96.0).ceil().max(1.0) as i32
}

/// Keep a manually moved window reachable while allowing it to cross monitors.
fn clamp_to_nearest_work_area(x: i32, y: i32, width: i32, height: i32) -> (i32, i32) {
    unsafe {
        let requested = RECT {
            left: x,
            top: y,
            right: x.saturating_add(width.max(1)),
            bottom: y.saturating_add(height.max(1)),
        };
        let monitor = MonitorFromRect(&requested, MONITOR_DEFAULTTONEAREST as u32);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if monitor.0.is_null() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return (x, y);
        }
        let max_x = info
            .rcWork
            .right
            .saturating_sub(width)
            .max(info.rcWork.left);
        let max_y = info
            .rcWork
            .bottom
            .saturating_sub(height)
            .max(info.rcWork.top);
        (
            x.clamp(info.rcWork.left, max_x),
            y.clamp(info.rcWork.top, max_y),
        )
    }
}

/// 动作 id → 高层 `UiAction`（越界 id 丢弃）。
fn ui_action(action: i32, index: i32) -> Option<UiAction> {
    Some(match action {
        ACTION_ITEM => UiAction::ItemInvoked(index as u32),
        ACTION_PREVIOUS => UiAction::NavigatePrevious,
        ACTION_NEXT => UiAction::NavigateNext,
        ACTION_EMOJI => UiAction::OpenEmojiPanel,
        ACTION_DISMISS => UiAction::Dismiss,
        _ => return None,
    })
}

/// 已展示内容：持有事件回调与最近一次快照（用于同内容快照的去重）。
struct Content {
    events: EventSink,
    last: CandidateView,
}

struct App {
    canvas: Rc<RefCell<crate::canvas::Canvas>>,
    runtime: WasmRuntime,
    /// 最近一帧命令流（`WM_PAINT` 回放）。
    frame: Vec<DrawCommand>,
    /// 主题最近声明的尺寸（DIP，`set_size` 导入）。
    size: (f32, f32),
    /// 已应用到窗口的尺寸（DIP），用于去重 `SetWindowPos`。
    applied: (f32, f32),
    anchor: Option<Anchor>,
    content: Option<Content>,
    /// 主题/宿主诊断（`take_notices` 排空）。
    notices: Vec<ThemeNotice>,
}

/// 稳定的分配对象比其 HWND 活得久：原生回调同步派发消息时，
/// 绝不持有它或 `App` 的可变借用。
pub struct Window {
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    app: RefCell<App>,
    error: RefCell<Option<String>>,
    recovery: RefCell<Recovery>,
    positioning: Cell<bool>,
    drag: Cell<Option<DragState>>,
    /// Physical screen position chosen by the user for this process lifetime.
    manual_position: Cell<Option<(i32, i32)>>,
    panel: Cell<crate::protocol::PanelStyle>,
    backdrop: Cell<crate::protocol::BackdropStyle>,
    anchor_rect: Cell<crate::protocol::Rect>,
    placement: Cell<PlacementStyle>,
    preview: bool,
    dark: Cell<bool>,
    wake_deadline: Cell<Option<f64>>,
}

impl Window {
    pub(crate) fn new(
        canvas: Rc<RefCell<crate::canvas::Canvas>>,
        runtime: WasmRuntime,
        preview: bool,
    ) -> Self {
        Self {
            hwnd: Cell::new(HWND::default()),
            dpi: Cell::new(96),
            app: RefCell::new(App {
                canvas,
                runtime,
                frame: Vec::new(),
                size: (0.0, 0.0),
                applied: (0.0, 0.0),
                anchor: None,
                content: None,
                notices: Vec::new(),
            }),
            error: RefCell::new(None),
            recovery: RefCell::new(Recovery::default()),
            positioning: Cell::new(false),
            drag: Cell::new(None),
            manual_position: Cell::new(None),
            panel: Cell::new(Default::default()),
            backdrop: Cell::new(Default::default()),
            anchor_rect: Cell::new(Default::default()),
            placement: Cell::new(Default::default()),
            preview,
            dark: Cell::new(appearance::is_dark()),
            wake_deadline: Cell::new(None),
        }
    }

    fn fail(&self, error: impl ToString) {
        self.cancel_wakeup();
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
        unsafe {
            if GetCapture() == self.hwnd.get() {
                let _ = ReleaseCapture();
            }
        }
    }

    fn begin_drag(&self) -> windows_core::Result<bool> {
        if !matches!(
            self.app.borrow().runtime.placement(),
            PlacementStyle::Fixed { .. }
        ) {
            return Ok(false);
        }
        let mut cursor = POINT::default();
        let mut rect = RECT::default();
        unsafe {
            GetCursorPos(&mut cursor).ok()?;
            GetWindowRect(self.hwnd.get(), &mut rect).ok()?;
            self.manual_position.set(Some((rect.left, rect.top)));
            self.drag.set(Some(DragState {
                cursor_x: cursor.x,
                cursor_y: cursor.y,
                window_x: rect.left,
                window_y: rect.top,
            }));
            let _ = SetCapture(self.hwnd.get());
        }
        Ok(true)
    }

    fn move_drag(&self) -> windows_core::Result<bool> {
        let Some(drag) = self.drag.get() else {
            return Ok(false);
        };
        let mut cursor = POINT::default();
        let mut rect = RECT::default();
        unsafe {
            GetCursorPos(&mut cursor).ok()?;
            GetWindowRect(self.hwnd.get(), &mut rect).ok()?;
        }
        let x = drag
            .window_x
            .saturating_add(cursor.x.saturating_sub(drag.cursor_x));
        let y = drag
            .window_y
            .saturating_add(cursor.y.saturating_sub(drag.cursor_y));
        let (x, y) = clamp_to_nearest_work_area(
            x,
            y,
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
        );
        self.manual_position.set(Some((x, y)));
        unsafe {
            SetWindowPos(
                self.hwnd.get(),
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                (SWP_NOSIZE | SWP_NOACTIVATE) as u32,
            )
            .ok()?;
        }
        Ok(true)
    }

    /// 按主题声明尺寸与锚点定位窗口（DPI 变化时有限次重算）。
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
        for _ in 0..3 {
            let dpi = self.dpi.get();
            let bounds = {
                let app = self.app.borrow();
                let (dip_w, dip_h) = app.size;
                if dip_w <= 0.0 || dip_h <= 0.0 {
                    None
                } else {
                    let edge = crate::geometry::insets(&app.runtime.panel_style());
                    let width = pixels(dip_w + edge.left + edge.right, dpi);
                    let height = pixels(dip_h + edge.top + edge.bottom, dpi);
                    let position = if let Some((x, y)) = self.manual_position.get() {
                        Some(clamp_to_nearest_work_area(x, y, width, height))
                    } else if self.preview {
                        Some(preview_position(width, height))
                    } else {
                        match app.runtime.placement() {
                            PlacementStyle::Fixed { x, y } => Some(
                                crate::presentation::fixed_position(x, y, width, height, dpi),
                            ),
                            PlacementStyle::Anchored => app
                                .anchor
                                .as_ref()
                                .filter(|anchor| anchor.valid)
                                .map(|anchor| {
                                    let anchor_rect = app.runtime.anchor_rect();
                                    let (x, y) = popup_position(
                                        anchor,
                                        pixels(anchor_rect.w, dpi),
                                        pixels(anchor_rect.h, dpi),
                                    );
                                    // Anchor belongs to the content, not the shadow-expanded surface.
                                    (
                                        x - ((edge.left + anchor_rect.x) * dpi as f32 / 96.0)
                                            .round()
                                            as i32,
                                        y - ((edge.top + anchor_rect.y) * dpi as f32 / 96.0).round()
                                            as i32,
                                    )
                                }),
                        }
                    };
                    position.map(|(x, y)| {
                        let (x, y) = clamp_to_nearest_work_area(x, y, width, height);
                        (x, y, width, height)
                    })
                }
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
            // 跨显示器移动可能同步改变 DPI：重算后重试，有限次数封顶。
            if dpi == self.dpi.get() {
                break;
            }
        }
        Ok(())
    }

    fn paint(&self) -> windows_core::Result<()> {
        let app = self.app.borrow();
        app.canvas
            .borrow_mut()
            .ensure_target(self.hwnd.get(), self.dpi.get())?;
        let mut canvas = app.canvas.borrow_mut();
        canvas.set_panel(app.runtime.panel_style(), app.size, self.dpi.get())?;
        canvas.set_backdrop(app.runtime.backdrop_style())?;
        canvas.replay(&app.frame)?;
        if app.runtime.visible() && app.content.is_some() {
            canvas.set_layers(app.runtime.layers())?;
        } else {
            canvas.clear_layers()?;
        }
        self.recovery.borrow_mut().succeeded();
        Ok(())
    }

    fn graphics_error(&self, error: windows_core::Error) {
        self.app
            .borrow_mut()
            .canvas
            .borrow_mut()
            .invalidate_target();
        if !self
            .recovery
            .borrow_mut()
            .failed(crate::canvas::Canvas::is_device_lost(&error))
        {
            self.fail(error);
            return;
        }
        unsafe {
            if SetTimer(Some(self.hwnd.get()), RETRY_TIMER, RETRY_INTERVAL_MS, None) == 0 {
                self.fail(windows_core::Error::from_thread());
            }
        }
    }

    /// 鼠标/帧/外观事件后的公共副作用：发送动作、尺寸变化重定位、
    /// 画布 resize、动画计时器、收集诊断。
    fn apply_side_effects(
        &self,
        actions: &[(i32, i32)],
        frame_requested: crate::animation::WakeRequest,
        notes: Vec<String>,
    ) -> Result<(), String> {
        for &(action, index) in actions {
            if let Some(ui) = ui_action(action, index) {
                let app = self.app.borrow();
                if let Some(content) = &app.content {
                    let allowed = match ui {
                        UiAction::ItemInvoked(i) => content
                            .last
                            .items
                            .get(i as usize)
                            .is_some_and(|v| v.enabled),
                        UiAction::NavigatePrevious => content.last.can_page_previous,
                        UiAction::NavigateNext => content.last.can_page_next,
                        UiAction::Dismiss => true,
                        UiAction::OpenEmojiPanel => true,
                    };
                    let events = content.events.clone();
                    drop(app);
                    if allowed {
                        events.send(ui);
                    }
                }
            }
        }
        let size_changed = {
            let mut app = self.app.borrow_mut();
            if app.size != (0.0, 0.0) && app.size != app.applied {
                app.applied = app.size;
                true
            } else {
                false
            }
        };
        let panel = self.app.borrow().runtime.panel_style();
        let panel_changed = self.panel.replace(panel) != panel;
        let backdrop = self.app.borrow().runtime.backdrop_style();
        if self.backdrop.replace(backdrop) != backdrop {
            self.invalidate();
        }
        let (anchor_rect, placement) = {
            let app = self.app.borrow();
            (app.runtime.anchor_rect(), app.runtime.placement())
        };
        let anchor_changed = self.anchor_rect.replace(anchor_rect) != anchor_rect;
        let placement_changed = self.placement.replace(placement) != placement;
        if size_changed || panel_changed || anchor_changed || placement_changed {
            self.position().map_err(|e| e.to_string())?;
            let (w, h) = {
                let app = self.app.borrow();
                let edge = crate::geometry::insets(&panel);
                (
                    pixels(app.size.0 + edge.left + edge.right, self.dpi.get()),
                    pixels(app.size.1 + edge.top + edge.bottom, self.dpi.get()),
                )
            };
            self.app
                .borrow_mut()
                .canvas
                .borrow_mut()
                .resize(w as u32, h as u32, self.dpi.get())
                .map_err(|e| e.to_string())?;
        }
        self.schedule_wakeup(frame_requested)?;
        if !notes.is_empty() {
            let mut app = self.app.borrow_mut();
            for note in notes {
                if app.notices.len() >= MAX_NOTICES {
                    app.notices.remove(0);
                }
                app.notices.push(ThemeNotice {
                    severity: NoticeSeverity::Info,
                    code: "wasm-theme".into(),
                    message: note,
                    details: String::new(),
                });
            }
        }
        Ok(())
    }

    fn cancel_wakeup(&self) {
        self.wake_deadline.set(None);
        unsafe {
            let _ = KillTimer(Some(self.hwnd.get()), FRAME_TIMER);
        }
    }

    fn schedule_wakeup(&self, request: crate::animation::WakeRequest) -> Result<(), String> {
        let old = self.wake_deadline.get();
        let next = if self.app.borrow().runtime.visible() && self.app.borrow().content.is_some() {
            request.merge(old)
        } else {
            None
        };
        if next == old {
            return Ok(());
        }
        self.cancel_wakeup();
        if let Some(deadline) = next {
            let delay = (deadline - now_ms())
                .ceil()
                .clamp(FRAME_INTERVAL_MS as f64, 86_400_000.0) as u32;
            unsafe {
                if SetTimer(Some(self.hwnd.get()), FRAME_TIMER, delay, None) == 0 {
                    return Err(windows_core::Error::from_thread().to_string());
                }
            }
            self.wake_deadline.set(Some(deadline));
        }
        Ok(())
    }

    fn sync_visibility(&self) {
        let visible = self.app.borrow().runtime.visible();
        if !visible {
            self.cancel_wakeup();
            if let Err(e) = self.app.borrow().canvas.borrow_mut().clear_layers() {
                self.fail(e);
            }
        }
        unsafe {
            let _ = ShowWindow(
                self.hwnd.get(),
                if visible {
                    if self.preview {
                        SW_SHOW
                    } else {
                        SW_SHOWNOACTIVATE
                    }
                } else {
                    SW_HIDE
                },
            );
        }
    }

    /// 渲染入口：同内容快照只重定位；新快照交给 wasm 重排。
    pub(crate) fn render(
        &self,
        snapshot: &CandidateView,
        events: &EventSink,
    ) -> Result<(), String> {
        let moved = {
            let app = self.app.borrow();
            app.content.as_ref().is_some_and(|c| {
                (is_visible(snapshot) || (app.runtime.resident() && snapshot.active))
                    && same_content(&c.last, snapshot)
            })
        };
        if moved {
            {
                let mut app = self.app.borrow_mut();
                if let Some(content) = app.content.as_mut() {
                    content.last = snapshot.clone();
                    content.events = events.clone();
                }
                app.anchor = snapshot.anchor.clone();
            }
            self.position().map_err(|e| e.to_string())?;
            return self.health();
        }
        self.health()?;
        let accepted = {
            let app = self.app.borrow();
            is_visible(snapshot) || (app.runtime.resident() && snapshot.active)
        };
        if !accepted {
            self.hide();
            return Ok(());
        }
        let (actions, frame_requested, notes, changed) = {
            let mut app = self.app.borrow_mut();
            // Appearance notifications may arrive while hidden. Init is only
            // called once, so synchronize the palette before the next render.
            let dark = appearance::is_dark();
            let mut appearance_changed = false;
            let mut wake = crate::animation::WakeRequest::default();
            if self.dark.get() != dark {
                app.runtime.refresh(dark)?;
                if let Some(frame) = app.runtime.take_frame() {
                    app.frame = frame;
                    app.size = app.runtime.size();
                    appearance_changed = true;
                }
                wake = app.runtime.take_frame_request();
                self.dark.set(dark);
            }
            app.runtime.render(snapshot)?;
            let commands = app.runtime.take_frame();
            let changed = commands.is_some() || appearance_changed;
            // No submission preserves the presented snapshot, including its action identity.
            if let Some(commands) = commands {
                app.frame = commands;
                app.size = app.runtime.size();
                if app.runtime.main_updated() {
                    if app.content.is_none() {
                        app.content = Some(Content {
                            events: events.clone(),
                            last: snapshot.clone(),
                        });
                    } else {
                        let content = app.content.as_mut().expect("checked above");
                        content.events = events.clone();
                        content.last = snapshot.clone();
                    }
                    app.anchor = snapshot.anchor.clone();
                }
            }
            (
                app.runtime.take_actions(),
                wake.then(app.runtime.take_frame_request()),
                app.runtime.take_notes(),
                changed,
            )
        };
        self.apply_side_effects(&actions, frame_requested, notes)?;
        self.sync_visibility();
        if changed {
            self.invalidate();
        }
        self.health()
    }

    pub(crate) fn hide(&self) {
        self.cancel_wakeup();
        if let Err(e) = self.app.borrow().canvas.borrow_mut().clear_layers() {
            self.fail(e);
        }
        let had_content = self.app.borrow_mut().content.take().is_some();
        self.drag.set(None);
        self.cancel();
        let mut app = self.app.borrow_mut();
        if had_content {
            if let Err(e) = app.runtime.hide() {
                app.notices.push(ThemeNotice {
                    severity: NoticeSeverity::Warning,
                    code: "wasm-theme".into(),
                    message: format!("theme hide failed: {e}"),
                    details: String::new(),
                });
            }
            let _ = app.runtime.take_frame();
            let _ = app.runtime.take_frame_request();
        }
        app.content = None;
        app.anchor = None;
        app.frame.clear();
        app.size = (0.0, 0.0);
        app.applied = (0.0, 0.0);
        drop(app);
        self.recovery.borrow_mut().waiting = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd.get()), RETRY_TIMER);
            let _ = KillTimer(Some(self.hwnd.get()), FRAME_TIMER);
            let _ = ShowWindow(self.hwnd.get(), SW_HIDE);
        }
    }

    /// 外观变化：通知主题（wasm 内部重排），随后回放最新命令。
    pub(crate) fn refresh_appearance(&self) -> Result<(), String> {
        self.health()?;
        let (actions, frame_requested, notes, changed) = {
            let mut app = self.app.borrow_mut();
            if app.content.is_none() {
                (Vec::new(), Default::default(), Vec::new(), false)
            } else {
                let dark = appearance::is_dark();
                app.runtime.refresh(dark)?;
                self.dark.set(dark);
                let commands = app.runtime.take_frame();
                let changed = commands.is_some();
                // An explicit empty submission clears; no submission retains.
                if let Some(commands) = commands {
                    app.frame = commands;
                }
                app.size = app.runtime.size();
                (
                    app.runtime.take_actions(),
                    app.runtime.take_frame_request(),
                    app.runtime.take_notes(),
                    changed,
                )
            }
        };
        self.apply_side_effects(&actions, frame_requested, notes)?;
        self.sync_visibility();
        if changed {
            self.invalidate();
        }
        self.health()
    }

    fn mouse(&self, kind: i32, param: LPARAM) -> Result<(), String> {
        if !self.app.borrow().content.is_some() {
            return Ok(());
        }
        let dpi = self.dpi.get() as f32;
        let edge = crate::geometry::insets(&self.app.borrow().runtime.panel_style());
        let x = (param.0 as u16 as i16) as f32 * 96.0 / dpi - edge.left;
        let y = ((param.0 >> 16) as u16 as i16) as f32 * 96.0 / dpi - edge.top;
        let (actions, frame_requested, notes, changed, drag_requested) = {
            let mut app = self.app.borrow_mut();
            app.runtime.mouse(kind, x, y)?;
            let commands = app.runtime.take_frame();
            // Presentation-only submissions also require an invalidation.
            let changed = commands.is_some();
            if let Some(commands) = commands {
                app.frame = commands;
            }
            app.size = app.runtime.size();
            (
                app.runtime.take_actions(),
                app.runtime.take_frame_request(),
                app.runtime.take_notes(),
                changed,
                app.runtime.take_drag_request(),
            )
        };
        if kind == MOUSE_DOWN {
            let dragging = drag_requested && self.begin_drag().map_err(|e| e.to_string())?;
            if !dragging {
                unsafe {
                    let _ = SetCapture(self.hwnd.get());
                }
            }
        } else if kind == MOUSE_UP {
            self.cancel();
        }
        self.apply_side_effects(&actions, frame_requested, notes)?;
        self.sync_visibility();
        if changed {
            self.invalidate();
        }
        Ok(())
    }

    fn tick_frame(&self) -> Result<(), String> {
        if self.app.borrow().content.is_none() || !self.app.borrow().runtime.visible() {
            return Ok(());
        }
        let (actions, frame_requested, notes, changed) = {
            let mut app = self.app.borrow_mut();
            app.runtime.frame(now_ms())?;
            let commands = app.runtime.take_frame();
            let changed = commands.is_some();
            if let Some(commands) = commands {
                app.frame = commands;
            }
            app.size = app.runtime.size();
            (
                app.runtime.take_actions(),
                app.runtime.take_frame_request(),
                app.runtime.take_notes(),
                changed,
            )
        };
        self.apply_side_effects(&actions, frame_requested, notes)?;
        self.sync_visibility();
        if changed {
            self.invalidate();
        }
        Ok(())
    }

    /// WM_SIZE：按实际客户区尺寸 resize 画布。
    fn resize(&self) -> windows_core::Result<()> {
        let mut rc = RECT::default();
        unsafe {
            GetClientRect(self.hwnd.get(), &mut rc).ok()?;
        }
        self.app.borrow_mut().canvas.borrow_mut().resize(
            (rc.right - rc.left).max(1) as u32,
            (rc.bottom - rc.top).max(1) as u32,
            self.dpi.get(),
        )
    }

    pub(crate) fn health(&self) -> Result<(), String> {
        self.error
            .borrow()
            .as_ref()
            .map_or(Ok(()), |e| Err(e.clone()))
    }

    pub(crate) fn take_notices(&self) -> Vec<ThemeNotice> {
        std::mem::take(&mut self.app.borrow_mut().notices)
    }
}

/// 注册窗口类并创建窗口（live：不可激活的工具窗口；preview：可关闭的预览窗）。
pub(crate) fn create_window(window: &Window) -> Result<(), String> {
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
            if RegisterClassW(&wc).0 == 0 {
                return Err(windows_core::Error::from_thread().to_string());
            }
        }
        let (ex_style, title) = if window.preview {
            (WS_EX_TOPMOST as u32, w!("Weasel-RS WASM 皮肤预览"))
        } else {
            (
                (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as u32,
                w!("Weasel candidates (WASM)"),
            )
        };
        let hwnd = CreateWindowExW(
            ex_style | WS_EX_NOREDIRECTIONBITMAP as u32,
            CLASS,
            title,
            WS_POPUP | if window.preview { WS_SYSMENU as u32 } else { 0 },
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            Some(window as *const Window as *const std::ffi::c_void),
        );
        if hwnd.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        window.hwnd.set(hwnd);
        window.dpi.set(GetDpiForWindow(hwnd).max(1));
        // 急切创建 render target：失败时工厂可在首次显示前回退。
        window
            .app
            .borrow_mut()
            .canvas
            .borrow_mut()
            .ensure_target(hwnd, window.dpi.get())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Drop for Window {
    fn drop(&mut self) {
        let hwnd = self.hwnd.get();
        if !hwnd.0.is_null() {
            unsafe {
                // 先摘钩再销毁：销毁期间不会有回调触碰正在释放的 App。
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                self.cancel();
                let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                let _ = KillTimer(Some(hwnd), FRAME_TIMER);
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // 拦截所有 Rust 回调（含 panic），保证 ABI 边界可恢复。
    match catch_unwind(AssertUnwindSafe(|| unsafe { dispatch(hwnd, msg, wp, lp) })) {
        Ok(result) => result,
        Err(_) => {
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Window;
                if let Some(window) = ptr.as_ref() {
                    if let Ok(mut error) = window.error.try_borrow_mut() {
                        *error = Some("theme_wasm native callback panicked".into());
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
                    if window.preview {
                        PostQuitMessage(0);
                        return LRESULT(0);
                    }
                    return DefWindowProcW(hwnd, msg, wp, lp);
                }
                WM_NCHITTEST => {
                    let mut point = POINT {
                        x: (lp.0 as u16 as i16) as i32,
                        y: ((lp.0 >> 16) as u16 as i16) as i32,
                    };
                    let _ = ScreenToClient(hwnd, &mut point);
                    let app = window.app.borrow();
                    let style = app.runtime.panel_style();
                    let edge = crate::geometry::insets(&style);
                    let scale = window.dpi.get() as f32 / 96.0;
                    let hit = app.runtime.hit_test(
                        point.x as f32 / scale - edge.left,
                        point.y as f32 / scale - edge.top,
                    ) >= 0;
                    // HTTRANSPARENT only delegates within this UI thread; do not
                    // synthesize input into arbitrary applications underneath.
                    return LRESULT(if hit { HTCLIENT } else { HTTRANSPARENT } as isize);
                }
                WM_MOUSEACTIVATE => {
                    if window.preview {
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
                    if window.drag.get().is_none() {
                        window.cancel();
                    }
                    if let Err(e) = window.position() {
                        window.fail(e);
                    }
                    if let Err(e) = window.resize() {
                        window.graphics_error(e);
                    }
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_TIMER if wp.0 == RETRY_TIMER as usize => {
                    let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                    window.recovery.borrow_mut().waiting = false;
                    window.invalidate();
                    return LRESULT(0);
                }
                WM_TIMER if wp.0 == FRAME_TIMER as usize => {
                    let deadline = window.wake_deadline.get();
                    window.cancel_wakeup();
                    // KillTimer不能撤回已投递消息；忽略取消后残留消息，过早的旧消息重新排队。
                    let Some(deadline) = deadline else {
                        return LRESULT(0);
                    };
                    if now_ms() < deadline {
                        if let Err(e) = window.schedule_wakeup(crate::animation::WakeRequest {
                            cancel: false,
                            deadline: Some(deadline),
                        }) {
                            window.fail(e);
                        }
                        return LRESULT(0);
                    }
                    if let Err(e) = window.tick_frame() {
                        window.fail(e);
                    }
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN => {
                    if let Err(e) = window.mouse(MOUSE_DOWN, lp) {
                        window.fail(e);
                    }
                    return LRESULT(0);
                }
                WM_LBUTTONUP => {
                    if window.drag.get().is_some() {
                        if let Err(e) = window.move_drag() {
                            window.fail(e);
                        }
                        window.drag.set(None);
                        window.cancel();
                        return LRESULT(0);
                    }
                    if let Err(e) = window.mouse(MOUSE_UP, lp) {
                        window.fail(e);
                    }
                    return LRESULT(0);
                }
                WM_MOUSEMOVE => {
                    match window.move_drag() {
                        Ok(true) => return LRESULT(0),
                        Ok(false) => {}
                        Err(e) => {
                            window.drag.set(None);
                            window.cancel();
                            window.fail(e);
                            return LRESULT(0);
                        }
                    }
                    if let Err(e) = window.mouse(MOUSE_MOVE, lp) {
                        window.fail(e);
                    }
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE as u32,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    return LRESULT(0);
                }
                WM_MOUSELEAVE if window.drag.get().is_some() => {
                    // Capture keeps delivering movement outside the old window
                    // rectangle; a pending TrackMouseEvent must not end the drag.
                    return LRESULT(0);
                }
                WM_MOUSELEAVE | WM_CAPTURECHANGED | WM_CANCELMODE => {
                    window.drag.set(None);
                    window.cancel();
                    let kind = if msg == WM_MOUSELEAVE as u32 {
                        MOUSE_LEAVE
                    } else {
                        crate::protocol::MOUSE_CANCEL
                    };
                    if let Err(e) = window.mouse(kind, lp) {
                        window.fail(e);
                    }
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
