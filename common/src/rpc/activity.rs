//! RPC 活动状态记录
//!
//! 记录 RPC 连接的状态，处理连接回收
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 一次连接退役检查的不可变快照。
///
/// 候选值充当 compare-and-set 令牌。真正退役时必须把原值传回连接；期间只要连接活动
/// 状态或最近请求时间发生变化，退役就会被拒绝。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RetireCandidate {
    /// 最近一次成功登记请求的时间，用于同级候选之间的 LRU 排序。
    pub last_request: Instant,
}

#[derive(Default)]
pub(super) struct Activity {
    state: Mutex<State>,
    notifier: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct State {
    /// 已从管道接收、但引擎尚未处理完的请求数量。
    requests: usize,
    /// 已进入发送队列、但尚未完成本地管道写入的帧数量。
    writes: usize,
    /// 由业务层确认该连接目前可参与基于时长的空闲回收。
    idle: bool,
    /// 退役后拒绝登记新请求和新写入；状态不可逆。
    retired: bool,
    /// 最近一次成功登记请求的时间，而非任意读写活动时间。
    last_request: Instant,
}
impl Default for State {
    fn default() -> Self {
        Self {
            requests: 0,
            writes: 0,
            idle: false,
            retired: false,
            last_request: Instant::now(),
        }
    }
}
impl Activity {
    /// 通知连接所有者重新检查活动状态。
    pub fn notify(&self) {
        let notify = self
            .notifier
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(notify) = notify {
            notify();
        }
    }
    /// 替换活动状态通知器，并立即触发一次检查。
    pub fn set_notifier(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *self.notifier.lock().unwrap_or_else(|p| p.into_inner()) = Some(notify);
        self.notify();
    }
    /// 登记一个新请求；已退役的连接拒绝新请求。
    pub fn request(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired {
            return false;
        }
        s.requests += 1;
        // The engine must re-authorize eviction after applying this request.
        s.idle = false;
        s.last_request = Instant::now();
        true
    }
    /// 结束一个已登记请求，并唤醒等待回收或 admission 的任务。
    pub fn finish_request(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        debug_assert!(state.requests > 0, "unbalanced RPC request lease");
        state.requests = state.requests.saturating_sub(1);
        drop(state);
        self.notify();
    }
    /// 登记一个待写帧；已退役的连接拒绝新写入。
    pub fn write(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired {
            return false;
        }
        s.writes += 1;
        true
    }
    /// 结束一个已登记写入，并唤醒等待者。
    pub fn finish_write(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        debug_assert!(state.writes > 0, "unbalanced RPC write lease");
        state.writes = state.writes.saturating_sub(1);
        drop(state);
        self.notify();
    }
    /// 设置普通空闲回收标记。
    pub fn set_idle(&self, idle: bool) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).idle = idle;
    }
    /// 获取当前可安全退役的连接快照。
    pub fn candidate(&self) -> Option<RetireCandidate> {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired || s.requests != 0 || s.writes != 0 {
            return None;
        }
        Some(RetireCandidate {
            last_request: s.last_request,
        })
    }
    /// 若候选快照仍有效，则原子地标记连接退役，再在状态锁外执行关闭动作。
    pub fn retire(&self, expected: RetireCandidate, close: impl FnOnce()) -> bool {
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let current = RetireCandidate {
                last_request: s.last_request,
            };
            if s.retired || s.requests != 0 || s.writes != 0 || current != expected {
                return false;
            }
            s.retired = true;
        }
        close();
        true
    }
    /// 返回满足普通空闲回收条件的最近请求时间。
    pub fn stale_since(&self, age: Duration) -> Option<Instant> {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        (!s.retired
            && s.idle
            && s.requests == 0
            && s.writes == 0
            && s.last_request.elapsed() >= age)
            .then_some(s.last_request)
    }
    /// 若普通空闲候选仍有效，则标记退役并在状态锁外关闭连接。
    pub fn evict(&self, age: Duration, close: impl FnOnce()) -> bool {
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.retired
                || !s.idle
                || s.requests != 0
                || s.writes != 0
                || s.last_request.elapsed() < age
            {
                return false;
            }
            s.retired = true;
        }
        close();
        true
    }
}

/// 持有请求处理期间的连接回收保护。
///
/// 该租约由接收路径创建并与请求一起交给调用方。租约使用共享引用保持活动状态存活；
/// 被丢弃时自动减少在途请求计数并通知观察者，因此应持有到请求处理结束。
pub struct RequestLease(pub(super) Arc<Activity>);
impl Drop for RequestLease {
    fn drop(&mut self) {
        self.0.finish_request();
    }
}

#[cfg(test)]
#[path = "tests/activity.rs"]
mod tests;
