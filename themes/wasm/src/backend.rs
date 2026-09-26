//! WASM 主题的 [`ThemeBackend`] 适配层，将主题后端调用委托给 [`Window`]。
//!
//! 适配器不复制渲染状态或原生资源。`Rc<Window>` 保持窗口分配地址稳定，供 HWND 的
//! 原生回调使用；Rc 和窗口资源均局限于创建它们的 UI 线程。

use crate::theme_api::{CandidateView, EventSink, ThemeBackend, ThemeNotice};
use crate::window::Window;
use std::rc::Rc;

/// 将宿主 [`ThemeBackend`] 接口转发到 WASM 原生窗口。
///
/// 该类型只拥有窗口的共享引用，渲染状态与生命周期由窗口自身管理。
pub struct WasmBackend {
    /// 稳定分配的窗口对象；其 HWND 回调在窗口销毁前指向此对象。
    window: Rc<Window>,
}

impl WasmBackend {
    /// 创建轻量后端句柄，不创建或复制窗口资源。
    pub(crate) fn new(window: Rc<Window>) -> Self {
        Self { window }
    }
}

impl ThemeBackend for WasmBackend {
    /// 取出并清空窗口积累的诊断通知。
    fn take_notices(&mut self) -> Vec<ThemeNotice> {
        self.window.take_notices()
    }
    /// 将候选快照及其事件接收端交给窗口渲染。
    ///
    /// 窗口会按内容是否变化决定是否调用 WASM 布局，并返回主题或原生副作用错误。
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        self.window.render(snapshot, events)
    }
    /// 隐藏当前内容并清理窗口侧的帧、输入捕获和唤醒状态。
    fn hide(&mut self) {
        self.window.hide();
    }
    /// 通知活动主题系统外观变化并更新当前帧。
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.window.refresh_appearance()
    }
    /// 检查窗口是否已记录不可恢复的错误。
    fn check_health(&mut self) -> Result<(), String> {
        self.window.health()
    }
}
