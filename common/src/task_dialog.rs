//! Windows Task Dialog 的安全所有权封装。
//!
//! [`TaskDialog`] 保存调用期间使用的 UTF-16 字符串，并将原始回调数据限制在
//! `TaskDialogIndirect` 的同步生命周期内。[`TaskDialogController`] 可以克隆到工作线程，
//! 用于更新文字、marquee 进度条和按钮状态；对话框创建前发生的更新会被缓存并在
//! `TDN_CREATED` 时应用。

use std::sync::{Arc, Mutex};

use windows_core::HRESULT;
use windows_strings::{HSTRING, PCWSTR};

use crate::bindings::*;

type HyperlinkHandler = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// Task Dialog 内置“确定”按钮的标识。
pub const BUTTON_OK: i32 = IDOK;

/// 可以由 [`TaskDialogController`] 动态更新的文字区域。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Element {
    /// 粗体主标题。
    MainInstruction,
    /// 主正文。
    Content,
    /// 展开区域，适合放置诊断或日志。
    ExpandedInformation,
    /// 底部脚注。
    Footer,
}

impl Element {
    fn native(self) -> usize {
        match self {
            Self::MainInstruction => TDE_MAIN_INSTRUCTION as usize,
            Self::Content => TDE_CONTENT as usize,
            Self::ExpandedInformation => TDE_EXPANDED_INFORMATION as usize,
            Self::Footer => TDE_FOOTER as usize,
        }
    }
}

#[derive(Clone)]
struct TextState {
    main_instruction: String,
    content: String,
    expanded_information: String,
    footer: String,
}

impl TextState {
    fn set(&mut self, element: Element, text: String) {
        match element {
            Element::MainInstruction => self.main_instruction = text,
            Element::Content => self.content = text,
            Element::ExpandedInformation => self.expanded_information = text,
            Element::Footer => self.footer = text,
        }
    }

    fn get(&self, element: Element) -> &str {
        match element {
            Element::MainInstruction => &self.main_instruction,
            Element::Content => &self.content,
            Element::ExpandedInformation => &self.expanded_information,
            Element::Footer => &self.footer,
        }
    }
}

struct LiveState {
    /// Task Dialog HWND；创建前和销毁后为零。
    hwnd: usize,
    text: TextState,
    marquee: bool,
    marquee_interval_ms: u32,
    progress_position: u32,
    ok_enabled: bool,
    hyperlink_handler: Option<HyperlinkHandler>,
}

struct SharedState(Mutex<LiveState>);

/// 可在线程间克隆的 Task Dialog 更新句柄。
///
/// 更新方法始终先写入缓存。如果对话框已经创建，还会同步向其窗口发送对应消息；
/// 因而调用结束意味着该次可见更新已经被 UI 线程处理。对话框销毁后的调用只更新缓存。
#[derive(Clone)]
pub struct TaskDialogController {
    state: Arc<SharedState>,
}

impl TaskDialogController {
    /// 更新指定文字区域。
    pub fn set_text(&self, element: Element, text: impl Into<String>) {
        let text = text.into();
        let hwnd = {
            let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.text.set(element, text.clone());
            state.hwnd
        };
        if hwnd != 0 {
            send_text(hwnd, element, &text);
        }
    }

    /// 启停 marquee 进度条，并指定动画更新间隔。
    ///
    /// `interval_ms` 为零时由系统选用默认速度。此方法不会改变对话框是否预留进度条；
    /// 创建 [`TaskDialog`] 时应通过 [`TaskDialog::marquee`] 启用该区域。
    pub fn set_marquee(&self, enabled: bool, interval_ms: u32) {
        let hwnd = {
            let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.marquee = enabled;
            state.marquee_interval_ms = interval_ms;
            state.hwnd
        };
        if hwnd != 0 {
            send_marquee(hwnd, enabled, interval_ms);
        }
    }

    /// 将默认 0–100 范围内的确定进度设置到指定位置。
    ///
    /// 通常先调用 [`set_marquee`](Self::set_marquee) 停止不确定动画，再设置最终位置。
    pub fn set_progress_position(&self, position: u32) {
        let position = position.min(100);
        let hwnd = {
            let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.progress_position = position;
            state.hwnd
        };
        if hwnd != 0 {
            send_progress_position(hwnd, position);
        }
    }

    /// 启用或禁用内置“确定”按钮。
    pub fn set_ok_enabled(&self, enabled: bool) {
        let hwnd = {
            let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.ok_enabled = enabled;
            state.hwnd
        };
        if hwnd != 0 {
            send_ok_enabled(hwnd, enabled);
        }
    }
}

/// Task Dialog 的声明式配置。
///
/// 当前封装提供最常用的单按钮对话框，并保留动态内容和进度控制能力。调用
/// [`controller`](Self::controller) 可在显示前取得工作线程使用的更新句柄。
pub struct TaskDialog {
    title: String,
    expanded_control_text: String,
    collapsed_control_text: String,
    expanded_by_default: bool,
    show_marquee: bool,
    enable_hyperlinks: bool,
    state: Arc<SharedState>,
}

/// Task Dialog 关闭时返回的用户选择。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDialogResult {
    /// 被点击的按钮标识；内置“确定”为 [`BUTTON_OK`]。
    pub button: i32,
    /// 被选择的单选按钮；未配置单选项时为零。
    pub radio_button: i32,
    /// 验证复选框的最终状态。
    pub verification_checked: bool,
}

impl TaskDialog {
    /// 创建仅含“确定”按钮的对话框。
    pub fn new(title: impl Into<String>, main_instruction: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            expanded_control_text: "显示详细信息".into(),
            collapsed_control_text: "隐藏详细信息".into(),
            expanded_by_default: false,
            show_marquee: false,
            enable_hyperlinks: false,
            state: Arc::new(SharedState(Mutex::new(LiveState {
                hwnd: 0,
                text: TextState {
                    main_instruction: main_instruction.into(),
                    content: String::new(),
                    expanded_information: String::new(),
                    footer: String::new(),
                },
                marquee: false,
                marquee_interval_ms: 0,
                progress_position: 0,
                ok_enabled: true,
                hyperlink_handler: None,
            }))),
        }
    }

    /// 设置主正文。
    pub fn content(self, text: impl Into<String>) -> Self {
        self.set_initial_text(Element::Content, text.into());
        self
    }

    /// 设置展开区域的初始内容。
    pub fn expanded_information(self, text: impl Into<String>) -> Self {
        self.set_initial_text(Element::ExpandedInformation, text.into());
        self
    }

    /// 设置展开和收起链接的文字。
    pub fn expansion_labels(
        mut self,
        expanded: impl Into<String>,
        collapsed: impl Into<String>,
    ) -> Self {
        self.expanded_control_text = expanded.into();
        self.collapsed_control_text = collapsed.into();
        self
    }

    /// 控制详细信息是否默认展开。
    pub fn expanded_by_default(mut self, enabled: bool) -> Self {
        self.expanded_by_default = enabled;
        self
    }

    /// 为对话框预留 marquee 进度条并设置初始运行状态。
    pub fn marquee(mut self, enabled: bool, interval_ms: u32) -> Self {
        self.show_marquee = true;
        {
            let mut state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.marquee = enabled;
            state.marquee_interval_ms = interval_ms;
        }
        self
    }

    /// 设置内置“确定”按钮的初始状态。
    pub fn ok_enabled(self, enabled: bool) -> Self {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ok_enabled = enabled;
        self
    }

    /// 启用正文中的 `<a href="…">…</a>` 标记并注册点击回调。
    ///
    /// 回调在 Task Dialog 所在线程执行，应该尽快返回；若需要启动进程或执行 I/O，
    /// 应将工作转交给其他线程。回调 panic 会在 FFI 边界内被捕获。
    pub fn on_hyperlink(mut self, handler: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.enable_hyperlinks = true;
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hyperlink_handler = Some(Arc::new(handler));
        self
    }

    /// 返回可供工作线程使用的动态更新句柄。
    pub fn controller(&self) -> TaskDialogController {
        TaskDialogController {
            state: self.state.clone(),
        }
    }

    /// 显示一次模态 Task Dialog，直到用户点击已启用的按钮。
    ///
    /// 消费配置对象可以避免同一份回调状态被并发用于多个原生窗口；此前取得的控制器
    /// 在对话框关闭后仍安全，但后续更新只会写入已经不可见的缓存。
    pub fn show(self) -> windows_core::Result<TaskDialogResult> {
        let initial = self
            .state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .text
            .clone();
        let title = HSTRING::from(&self.title);
        let main = HSTRING::from(initial.main_instruction);
        let content = HSTRING::from(initial.content);
        let expanded = HSTRING::from(initial.expanded_information);
        let footer = HSTRING::from(initial.footer);
        let expanded_control = HSTRING::from(&self.expanded_control_text);
        let collapsed_control = HSTRING::from(&self.collapsed_control_text);
        let mut flags = TDF_SIZE_TO_CONTENT;
        if self.expanded_by_default {
            flags |= TDF_EXPANDED_BY_DEFAULT;
        }
        if self.show_marquee {
            flags |= TDF_SHOW_MARQUEE_PROGRESS_BAR;
        }
        if self.enable_hyperlinks {
            flags |= TDF_ENABLE_HYPERLINKS;
        }
        let config = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: flags,
            dwCommonButtons: TDCBF_OK_BUTTON,
            pszWindowTitle: PCWSTR(title.as_ptr()),
            pszMainInstruction: PCWSTR(main.as_ptr()),
            pszContent: PCWSTR(content.as_ptr()),
            pszExpandedInformation: PCWSTR(expanded.as_ptr()),
            pszExpandedControlText: PCWSTR(expanded_control.as_ptr()),
            pszCollapsedControlText: PCWSTR(collapsed_control.as_ptr()),
            pszFooter: PCWSTR(footer.as_ptr()),
            pfCallback: Some(task_dialog_callback),
            lpCallbackData: Arc::as_ptr(&self.state) as isize,
            ..Default::default()
        };
        let mut button = 0;
        let mut radio_button = 0;
        let mut verification_checked = windows_core::BOOL::default();
        unsafe {
            TaskDialogIndirect(
                &config,
                Some(&mut button),
                Some(&mut radio_button),
                Some(&mut verification_checked),
            )
            .ok()?;
        }
        Ok(TaskDialogResult {
            button,
            radio_button,
            verification_checked: verification_checked.as_bool(),
        })
    }

    fn set_initial_text(&self, element: Element, text: String) {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .text
            .set(element, text);
    }
}

unsafe extern "system" fn task_dialog_callback(
    hwnd: HWND,
    notification: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
    callback_data: isize,
) -> HRESULT {
    // `show` keeps the owning Arc alive for the complete synchronous
    // TaskDialogIndirect call, including every callback notification.
    let state = unsafe { &*(callback_data as *const SharedState) };
    if notification == TDN_CREATED as u32 {
        let (text, marquee, interval, progress_position, ok_enabled) = {
            let mut state = state.0.lock().unwrap_or_else(|p| p.into_inner());
            state.hwnd = hwnd.0 as usize;
            (
                state.text.clone(),
                state.marquee,
                state.marquee_interval_ms,
                state.progress_position,
                state.ok_enabled,
            )
        };
        for element in [
            Element::MainInstruction,
            Element::Content,
            Element::ExpandedInformation,
            Element::Footer,
        ] {
            send_text(hwnd.0 as usize, element, text.get(element));
        }
        send_marquee(hwnd.0 as usize, marquee, interval);
        send_progress_position(hwnd.0 as usize, progress_position);
        send_ok_enabled(hwnd.0 as usize, ok_enabled);
    } else if notification == TDN_DESTROYED as u32 {
        state.0.lock().unwrap_or_else(|p| p.into_inner()).hwnd = 0;
    } else if notification == TDN_HYPERLINK_CLICKED as u32 {
        let handler = state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hyperlink_handler
            .clone();
        let target = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
        if let (Some(handler), Ok(target)) = (handler, target) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&target)));
        }
    }
    HRESULT(0)
}

fn send_text(hwnd: usize, element: Element, text: &str) {
    let text = HSTRING::from(text);
    unsafe {
        SendMessageW(
            HWND(hwnd as *mut _),
            TDM_SET_ELEMENT_TEXT as u32,
            WPARAM(element.native()),
            LPARAM(text.as_ptr() as isize),
        );
    }
}

fn send_marquee(hwnd: usize, enabled: bool, interval_ms: u32) {
    unsafe {
        SendMessageW(
            HWND(hwnd as *mut _),
            TDM_SET_MARQUEE_PROGRESS_BAR as u32,
            WPARAM(enabled as usize),
            LPARAM(0),
        );
        SendMessageW(
            HWND(hwnd as *mut _),
            TDM_SET_PROGRESS_BAR_MARQUEE as u32,
            WPARAM(enabled as usize),
            LPARAM(interval_ms as isize),
        );
    }
}

fn send_progress_position(hwnd: usize, position: u32) {
    unsafe {
        SendMessageW(
            HWND(hwnd as *mut _),
            TDM_SET_PROGRESS_BAR_POS as u32,
            WPARAM(position as usize),
            LPARAM(0),
        );
    }
}

fn send_ok_enabled(hwnd: usize, enabled: bool) {
    unsafe {
        SendMessageW(
            HWND(hwnd as *mut _),
            TDM_ENABLE_BUTTON as u32,
            WPARAM(BUTTON_OK as usize),
            LPARAM(enabled as isize),
        );
    }
}
