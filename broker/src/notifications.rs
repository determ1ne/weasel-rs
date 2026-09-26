//! 集中管理 broker 通知策略。
//!
//! 每条通知都会记录到日志；每个生命周期至多尝试展示一次桌面通知，避免
//! 配置加载等后台路径反复打断用户。重置生命周期时可重新允许展示。
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use weasel_common::{logging::ComponentLogger, message::UserNotification, process::RuntimePaths};

#[derive(Clone)]
/// 管理 broker 通知的记录与展示策略，可在线程间共享。
///
/// 克隆值共享同一状态，因此通知限流和日志记录器也在所有克隆之间共享。
pub struct NotificationCenter(Arc<State>);

/// 通知中心的共享状态；原子标记负责跨线程限流，互斥锁保护日志记录器。
struct State {
    /// 当前生命周期是否已尝试向用户展示通知。
    shown: AtomicBool,
    /// 串行化对组件日志记录器的访问。
    logger: Mutex<ComponentLogger>,
    /// 保持最近一次 Toast 及其激活处理器存活。
    active_toast: Mutex<Option<crate::toast::ActiveToast>>,
}

/// broker 允许用户从通知中确认执行的受控动作。
#[derive(Clone, Copy, Debug)]
pub(crate) enum NotificationAction {
    /// 打开 Rime 用户数据目录，以便修复 `weasel.custom.json`。
    OpenUserDirectory,
}

impl NotificationAction {
    /// 返回 Toast 操作按钮使用的本地化文字。
    fn label(self) -> &'static str {
        match self {
            Self::OpenUserDirectory => "打开用户配置文件夹",
        }
    }

    /// 异步执行动作，避免阻塞 WinRT 激活回调或通知工作线程。
    fn invoke(self) {
        match self {
            Self::OpenUserDirectory => {
                crate::menu_actions::open(weasel_common::command_menu::USER_DIRECTORY)
            }
        }
    }
}
impl NotificationCenter {
    /// 创建通知中心，并将通知日志写入运行时日志目录。
    ///
    /// 测试环境使用标准错误输出；文件日志不可用时回退到标准错误，并继续创建。
    pub fn new(paths: &RuntimePaths) -> Self {
        let logger = if cfg!(test) {
            ComponentLogger::stderr()
        } else {
            let (logger, error) =
                ComponentLogger::file_or_stderr(&paths.logs, "broker-notifications");
            if let Some(error) = error {
                eprintln!("weasel-broker: notification log unavailable: {error}");
            }
            logger
        };
        Self(Arc::new(State {
            shown: AtomicBool::new(false),
            logger: Mutex::new(logger),
            active_toast: Mutex::new(None),
        }))
    }

    /// 开始新的通知生命周期，允许再次尝试展示一条通知。
    ///
    /// 此操作不会清除已有日志，也不会影响正在进行的通知展示。
    pub fn reset(&self) {
        self.0.shown.store(false, Ordering::Release);
    }

    /// 在设置加载警告非空时报告一次配置加载失败通知。
    ///
    /// 详细警告原样合并到通知详情中；没有警告时不记录也不展示通知。
    pub fn report_settings_errors(&self, warnings: &[String]) {
        if warnings.is_empty() {
            return;
        }
        self.report_with_action(
            UserNotification {
                source: "broker".into(),
                code: "settings.load_failed".into(),
                severity: weasel_common::message::UserNotificationSeverity::Warning as i32,
                title: "小狼毫RS：配置加载失败".into(),
                message: "配置文件存在错误。请检查 weasel.custom.json。".into(),
                details: warnings.join("\n"),
            },
            Some(NotificationAction::OpenUserDirectory),
        );
    }

    /// 记录通知，并在本生命周期首次调用时尝试向用户展示。
    ///
    /// 后续调用仍会逐条记日志，但不会再次弹出界面。桌面 Toast 失败时会在独立
    /// 工作线程中尝试便携式消息框，避免阻塞调用方的 RPC 或文件系统工作线程；
    /// 测试环境只记录通知，不调用系统界面。
    pub fn report(&self, notice: UserNotification) {
        let action = (notice.code == "configuration.invalid")
            .then_some(NotificationAction::OpenUserDirectory);
        self.report_with_action(notice, action);
    }

    /// 记录通知，并为 broker 自己生成的通知附加受控回执动作。
    pub(crate) fn report_with_action(
        &self,
        notice: UserNotification,
        action: Option<NotificationAction>,
    ) {
        self.log_at(
            match weasel_common::message::UserNotificationSeverity::try_from(notice.severity) {
                Ok(weasel_common::message::UserNotificationSeverity::Error) => {
                    weasel_common::logging::Level::ERROR
                }
                Ok(weasel_common::message::UserNotificationSeverity::Warning) => {
                    weasel_common::logging::Level::WARN
                }
                _ => weasel_common::logging::Level::INFO,
            },
            &format!(
                "source={:?} code={:?} severity={} title={:?} message={:?} details={:?}",
                notice.source,
                notice.code,
                notice.severity,
                notice.title,
                notice.message,
                notice.details
            ),
        );
        // 每次生命周期中只显示一次通知。
        if !self.0.shown.swap(true, Ordering::AcqRel) && !cfg!(test) {
            let toast_action = action.map(|action| crate::toast::ToastAction {
                label: action.label(),
                invoke: Box::new(move || action.invoke()),
            });
            match crate::toast::show(&notice.title, &notice.message, toast_action) {
                Ok(toast) => {
                    *self
                        .0
                        .active_toast
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(toast);
                }
                Err(error) => {
                    self.log(&format!("Toast failed: {error}"));
                    // 桌面 Toast 失败时，尝试使用便携式消息框通知用户。
                    let center = self.clone();
                    if let Err(error) = std::thread::Builder::new()
                        .name("broker-notification-dialog".into())
                        .spawn(move || unsafe {
                            use crate::bindings::*;
                            use windows_strings::HSTRING;
                            let (instruction, buttons) = if let Some(action) = action {
                                (format!("\n\n单击“确定”以{}。", action.label()), MB_OKCANCEL)
                            } else {
                                (String::new(), MB_OK)
                            };
                            let result = MessageBoxW(
                                None,
                                &HSTRING::from(format!(
                                    "{}{}\n\n详情请查看 broker-notifications.*.log。",
                                    notice.message, instruction
                                )),
                                &HSTRING::from(notice.title),
                                (buttons | MB_ICONINFORMATION | MB_SETFOREGROUND) as u32,
                            );
                            if result == 0 {
                                center.log("notification MessageBox failed");
                            } else if result == IDOK
                                && let Some(action) = action
                            {
                                action.invoke();
                            }
                        })
                    {
                        self.log(&format!("notification dialog thread failed: {error}"));
                    }
                }
            }
        }
    }

    /// 以错误级别记录内部通知展示故障。
    fn log(&self, text: &str) {
        self.log_at(weasel_common::logging::Level::ERROR, text);
    }

    /// 在持有日志锁期间写入一条 broker 组件日志。
    ///
    /// 若互斥锁曾发生中毒，则取回其内部记录器继续记录，避免日志故障阻断通知流程。
    fn log_at(&self, level: weasel_common::logging::Level, text: &str) {
        let logger = self.0.logger.lock().unwrap_or_else(|p| p.into_inner());
        logger.record(level, "weasel-broker", format_args!("{text}"));
    }
}
