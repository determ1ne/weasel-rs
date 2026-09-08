//! Bounded asynchronous reporting; never perform pipe I/O on the UI thread.
use std::{
    sync::{OnceLock, mpsc},
    time::Duration,
};
use weasel_common::{
    message::{PeerRole, UserNotification, UserNotificationSeverity},
    rpc::{RpcClient, try_default_broker_pipe_name},
};

pub fn report(theme: &str, notice: crate::theme_api::ThemeNotice) {
    let diagnostic = UserNotification {
        source: "renderer".into(),
        code: bounded(&notice.code, 128),
        severity: match notice.severity {
            crate::theme_api::NoticeSeverity::Info => UserNotificationSeverity::Info,
            crate::theme_api::NoticeSeverity::Warning => UserNotificationSeverity::Warning,
            crate::theme_api::NoticeSeverity::Error => UserNotificationSeverity::Error,
        } as i32,
        title: "小狼毫RS：主题通知".into(),
        message: bounded(&notice.message, 2048),
        details: bounded(&format!("theme={theme}: {}", notice.details), 16384),
    };
    crate::diagnostics::record(format_args!(
        "theme notice: {} details={}",
        diagnostic.message, diagnostic.details
    ));
    // Unit tests validate parsing only, without contacting a running broker.
    if cfg!(test) {
        return;
    }
    static SENDER: OnceLock<Option<mpsc::SyncSender<UserNotification>>> = OnceLock::new();
    let sender = SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("theme-diagnostics".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                for report in rx {
                    let result = runtime.block_on(async {
                        tokio::time::timeout(Duration::from_secs(3), async {
                            let pipe = try_default_broker_pipe_name()?;
                            let client = RpcClient::connect_as(pipe, PeerRole::Renderer).await?;
                            let result = client.notify_user(report).await;
                            client.disconnect().await;
                            result
                        })
                        .await
                    });
                    if !matches!(result, Ok(Ok(()))) {
                        crate::diagnostics::record(format_args!(
                            "broker diagnostic delivery failed: {result:?}"
                        ));
                    }
                }
            })
            .map_err(|error| {
                crate::diagnostics::record(format_args!("diagnostic worker unavailable: {error}"))
            })
            .ok()?;
        Some(tx)
    });
    if sender
        .as_ref()
        .is_none_or(|s| s.try_send(diagnostic).is_err())
    {
        crate::diagnostics::record(format_args!(
            "broker diagnostic queue unavailable or full; details retained locally"
        ));
    }
}

fn bounded(value: &str, limit: usize) -> String {
    let value = value.replace('\0', "");
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}

pub fn drain(theme: &str, backend: &mut dyn crate::theme_api::ThemeBackend) {
    for notice in backend.take_notices() {
        report(theme, notice);
    }
}
