//! 将主题通知记录到本地诊断日志，并异步转发给 broker。

use std::{
    sync::{OnceLock, mpsc},
    time::Duration,
};
use weasel_common::{
    message::{PeerRole, UserNotification, UserNotificationSeverity, envelope::Payload},
    rpc::{RpcClient, RpcError, try_default_broker_pipe_name},
};

/// 记录主题通知，并尽力将长度受限的消息异步发送给 broker。
///
/// 首次调用时惰性创建唯一后台工作线程；队列最多缓存 32 条，调用方不会等待
/// 队列空间。测试构建只记录通知而不连接 broker。
pub fn report(theme: &str, notice: crate::theme_api::ThemeNotice) {
    let level = match notice.severity {
        crate::theme_api::NoticeSeverity::Info => weasel_common::logging::Level::INFO,
        crate::theme_api::NoticeSeverity::Warning => weasel_common::logging::Level::WARN,
        crate::theme_api::NoticeSeverity::Error => weasel_common::logging::Level::ERROR,
    };
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
    crate::diagnostics::record_at(
        level,
        format_args!(
            "theme notice: {} details={}",
            diagnostic.message, diagnostic.details
        ),
    );
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
                            let result = match client
                                .request(Payload::UserNotification(report))
                                .await?
                                .payload
                            {
                                Some(Payload::Pong(_)) => Ok(()),
                                _ => Err(RpcError::UnexpectedResponse),
                            };
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

/// 以警告级别报告主题不可用及其加载错误。
pub fn theme_unavailable(theme: &str, error: &str) {
    report(
        theme,
        crate::theme_api::ThemeNotice {
            severity: crate::theme_api::NoticeSeverity::Warning,
            code: "theme.unavailable".into(),
            message: format!("主题 {theme} 加载失败，正在尝试其他主题。"),
            details: format!("theme {theme} unavailable: {error}"),
        },
    );
}

/// 报告 renderer 消费的全局配置无效，并说明已经采用内置回退值。
pub fn invalid_configuration(details: &str) {
    report(
        "renderer",
        crate::theme_api::ThemeNotice {
            severity: crate::theme_api::NoticeSeverity::Warning,
            code: "configuration.invalid".into(),
            message: "渲染配置无效，已使用内置默认值。".into(),
            details: details.into(),
        },
    );
}

/// 移除 NUL 并按字节上限截断字符串，同时保证截断位置位于 UTF-8 字符边界。
fn bounded(value: &str, limit: usize) -> String {
    let value = value.replace('\0', "");
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}

/// 取出后端当前积累的通知，并逐条交由异步报告通道处理。
pub fn drain(theme: &str, backend: &mut dyn crate::theme_api::ThemeBackend) {
    for notice in backend.take_notices() {
        report(theme, notice);
    }
}
