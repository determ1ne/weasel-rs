//! WASM 主题的 ThemeBackend 实现：对 `window.rs` 的薄封装。
//!
//! 后端本身无状态；全部原生资源由 `Rc<Window>` 持有并钉在 UI 线程上，
//! 保证 `ThemeBackend` 经 `&mut self` 调用时 HWND 指针保持稳定。

use crate::theme_api::{CandidateView, EventSink, ThemeBackend, ThemeNotice};
use crate::window::Window;
use std::rc::Rc;

pub struct WasmBackend {
    window: Rc<Window>,
}

impl WasmBackend {
    pub(crate) fn new(window: Rc<Window>) -> Self {
        Self { window }
    }
}

impl ThemeBackend for WasmBackend {
    fn take_notices(&mut self) -> Vec<ThemeNotice> {
        self.window.take_notices()
    }
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        self.window.render(snapshot, events)
    }
    fn hide(&mut self) {
        self.window.hide();
    }
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.window.refresh_appearance()
    }
    fn check_health(&mut self) -> Result<(), String> {
        self.window.health()
    }
}
