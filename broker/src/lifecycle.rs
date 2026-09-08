use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Idle,
    Deploy,
    Restart,
    Failed,
    Shutdown,
}

impl Operation {
    pub fn monitors_server(self) -> bool {
        self == Self::Idle
    }

    pub fn monitors_renderer(self) -> bool {
        matches!(self, Self::Idle | Self::Deploy)
    }
}

/// Retry indefinitely at a capped rate; a healthy minute clears crash history.
pub struct RestartBackoff {
    failures: u32,
    next: Instant,
    started: Instant,
}

impl RestartBackoff {
    pub fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            next: now,
            started: now,
        }
    }

    pub fn failed(&mut self, now: Instant) {
        self.next = now + Duration::from_secs(1u64 << self.failures.min(5));
        self.failures = self.failures.saturating_add(1);
    }

    pub fn ready(&self, now: Instant) -> bool {
        now >= self.next
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    pub fn started(&mut self, now: Instant) {
        self.started = now;
    }

    pub fn healthy(&mut self, now: Instant) {
        if now.duration_since(self.started) >= Duration::from_secs(60) {
            self.failures = 0;
        }
    }
}

pub fn completion_result(success: bool, code: Option<i32>, message: &str) -> Result<(), String> {
    if success && code.is_none_or(|code| code == 0) {
        Ok(())
    } else {
        Err(format!("deployment failed (exit={code:?}): {message}"))
    }
}

/// Only an observation of our own child can override a failed shutdown RPC.
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
