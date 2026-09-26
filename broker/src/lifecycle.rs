//! broker 生命周期状态、服务重启退避以及部署和关闭结果的归并规则。
//!
//! 这些纯状态逻辑让监督器能依据当前操作决定监控范围，并保留足够的错误信息
//! 向调用方报告部署、恢复和子进程退出结果。
use std::time::{Duration, Instant};

/// broker 当前执行的操作，决定 server 与 renderer 是否继续受监督。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    /// 空闲状态；同时监控 server 和 renderer。
    #[default]
    Idle,
    /// 部署过程中暂停 server 监控，但继续监控 renderer。
    Deploy,
    /// 正在重启服务，不执行常规存活监控。
    Restart,
    /// 生命周期操作已失败。
    Failed,
    /// 正在关闭 broker。
    Shutdown,
}

impl Operation {
    /// 当前操作是否允许监督 server。
    pub fn monitors_server(self) -> bool {
        self == Self::Idle
    }

    /// 当前操作是否允许监督 renderer。
    pub fn monitors_renderer(self) -> bool {
        matches!(self, Self::Idle | Self::Deploy)
    }
}

/// 服务重启的退避状态。
///
/// 失败后延迟按 1、2、4……秒增长，最大为 32 秒且不限制重试次数；服务连续稳定
/// 至少一分钟后才清除失败计数。
pub struct RestartBackoff {
    /// 已累计的失败次数，采用饱和递增。
    failures: u32,
    /// 下一次允许重试的单调时钟时刻。
    next: Instant,
    /// 最近一次启动尝试的时刻，用于判断稳定运行时长。
    started: Instant,
}

impl RestartBackoff {
    /// 创建立即可重试、失败计数为零的退避状态。
    pub fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            next: now,
            started: now,
        }
    }

    /// 记录一次失败，并按指数退避更新下一次重试时刻；延迟上限为 32 秒。
    pub fn failed(&mut self, now: Instant) {
        self.next = now + Duration::from_secs(1u64 << self.failures.min(5));
        self.failures = self.failures.saturating_add(1);
    }

    /// 判断给定时刻是否已到下一次重试时间。
    pub fn ready(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// 返回距下一次重试的剩余时长；已到期时为零。
    pub fn remaining(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    /// 记录最近一次启动尝试的时刻，作为后续稳定运行计时的起点。
    pub fn started(&mut self, now: Instant) {
        self.started = now;
    }

    /// 若本次运行已稳定至少 60 秒，则清除历史失败计数。
    pub fn healthy(&mut self, now: Instant) {
        if now.duration_since(self.started) >= Duration::from_secs(60) {
            self.failures = 0;
        }
    }
}

/// 根据部署进程的成功标志和退出码判断部署结果。
///
/// 只有成功标志为真且退出码缺失或为零时才返回成功；其他组合均保留退出码和消息
/// 生成错误，避免仅凭进程 API 的成功标志掩盖非零退出。
pub fn completion_result(success: bool, code: Option<i32>, message: &str) -> Result<(), String> {
    if success && code.is_none_or(|code| code == 0) {
        Ok(())
    } else {
        Err(format!("deployment failed (exit={code:?}): {message}"))
    }
}

/// 合并关闭 RPC 错误与对受管子进程退出状态的观察结果。
///
/// 只有确认本 broker 的子进程已经退出，才可将 RPC 失败视为关闭成功；若子进程仍在
/// 运行则保留 RPC 错误，若无法检查则同时报告两类错误。
pub fn shutdown_error_result(
    rpc_error: String,
    child_exited: Result<bool, String>,
) -> Result<(), String> {
    match child_exited {
        Ok(true) => Ok(()),
        Ok(false) => Err(rpc_error),
        Err(error) => Err(format!("{rpc_error}; child exit check failed: {error}")),
    }
}

/// 合并部署与服务恢复结果，确保任一阶段的失败都不会被另一阶段的成功覆盖。
pub fn workflow_result(
    deployment: Result<(), String>,
    restoration: Result<(), String>,
) -> Result<(), String> {
    match (deployment, restoration) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(format!("{error}\nService restored.")),
        (Ok(()), Err(error)) => Err(format!(
            "Deployment succeeded; service restoration failed: {error}"
        )),
        (Err(deploy), Err(restore)) => {
            Err(format!("{deploy}\nService restoration failed: {restore}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_rpc_failure_requires_confirmed_child_exit() {
        assert!(shutdown_error_result("pipe disconnected".into(), Ok(true)).is_ok());
        assert_eq!(
            shutdown_error_result("pipe disconnected".into(), Ok(false)),
            Err("pipe disconnected".into())
        );
        let error =
            shutdown_error_result("RPC timed out".into(), Err("cannot inspect child".into()))
                .unwrap_err();
        assert!(error.contains("RPC timed out"));
        assert!(error.contains("cannot inspect child"));
    }

    #[test]
    fn deployment_preserves_renderer_supervision() {
        assert!(!Operation::Deploy.monitors_server());
        assert!(Operation::Deploy.monitors_renderer());
        for operation in [Operation::Restart, Operation::Shutdown] {
            assert!(!operation.monitors_server());
            assert!(!operation.monitors_renderer());
        }
    }

    #[test]
    fn backoff_caps_and_resets_only_after_stability() {
        let now = Instant::now();
        let mut retry = RestartBackoff::new(now);
        for delay in [1, 2, 4, 8, 16, 32, 32] {
            retry.failed(now);
            assert!(!retry.ready(now + Duration::from_secs(delay - 1)));
            assert!(retry.ready(now + Duration::from_secs(delay)));
        }
        retry.started(now);
        retry.healthy(now + Duration::from_secs(59));
        assert_ne!(retry.failures, 0);
        retry.healthy(now + Duration::from_secs(60));
        retry.failed(now);
        assert!(retry.ready(now + Duration::from_secs(1)));
    }

    #[test]
    fn completion_is_not_necessarily_success() {
        assert!(completion_result(false, Some(1), "failed").is_err());
        assert!(completion_result(true, Some(1), "inconsistent").is_err());
        assert!(completion_result(true, Some(0), "done").is_ok());
        assert!(
            workflow_result(Err("deploy failed".into()), Ok(()))
                .unwrap_err()
                .contains("deploy failed")
        );
        assert!(
            workflow_result(Ok(()), Err("ping failed".into()))
                .unwrap_err()
                .contains("Deployment succeeded")
        );
    }
}
