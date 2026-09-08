//! Central notification policy, independent of the reporting component and UI.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use weasel_common::{
    logging::ComponentLogger, message::UserNotification, runtime_paths::RuntimePaths,
};

#[derive(Clone)]
pub struct NotificationCenter(Arc<State>);
struct State {
    shown: AtomicBool,
    logger: Mutex<ComponentLogger>,
}
impl NotificationCenter {
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
        }))
    }
    pub fn reset(&self) {
        self.0.shown.store(false, Ordering::Release);
    }
    pub fn report_settings_errors(&self, warnings: &[String]) {
        if warnings.is_empty() {
            return;
        }
        self.report(UserNotification {
            source: "broker".into(),
            code: "settings.load_failed".into(),
            severity: weasel_common::message::UserNotificationSeverity::Warning as i32,
            title: "小狼毫RS：配置加载失败".into(),
            message: "配置文件存在错误，本次修改未生效。请检查 weasel.custom.json 或 weasel.json。"
                .into(),
            details: warnings.join("\n"),
        });
    }
    pub fn report(&self, notice: UserNotification) {
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
        // Retain every report, but attempt user notification once per lifecycle.
        if !self.0.shown.swap(true, Ordering::AcqRel) && !cfg!(test) {
            if let Err(error) = crate::toast::show(&notice.title, &notice.message) {
                self.log(&format!("Toast failed: {error}"));
                // A portable copy may have no installed notification identity.
                // Do not keep the RPC/filesystem worker occupied until dismissal.
                let center = self.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("broker-notification-dialog".into())
                    .spawn(move || unsafe {
                        use crate::bindings::*;
                        use windows_strings::HSTRING;
                        let result = MessageBoxW(
                            None,
                            &HSTRING::from(format!(
                                "{}\n\n详情请查看 broker-notifications.*.log。",
                                notice.message
                            )),
                            &HSTRING::from(notice.title),
                            (MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND) as u32,
                        );
                        if result == 0 {
                            center.log("notification MessageBox failed");
                        }
                    })
                {
                    self.log(&format!("notification dialog thread failed: {error}"));
                }
            }
        }
    }
    fn log(&self, text: &str) {
        self.log_at(weasel_common::logging::Level::ERROR, text);
    }
    fn log_at(&self, level: weasel_common::logging::Level, text: &str) {
        let logger = self.0.logger.lock().unwrap_or_else(|p| p.into_inner());
        logger.record(level, "weasel-broker", format_args!("{text}"));
    }
}
