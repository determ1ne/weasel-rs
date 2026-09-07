//! Coalesced engine wakeups and conservative idle-connection accounting.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Activity {
    state: Mutex<State>,
    notifier: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct State {
    requests: usize,
    writes: usize,
    idle: bool,
    input: bool,
    focused: bool,
    composing: bool,
    retired: bool,
    touched: Instant,
}
impl Default for State {
    fn default() -> Self {
        Self {
            requests: 0,
            writes: 0,
            idle: false,
            input: false,
            focused: false,
            composing: false,
            retired: false,
            touched: Instant::now(),
        }
    }
}
impl Activity {
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
    pub fn set_notifier(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *self.notifier.lock().unwrap_or_else(|p| p.into_inner()) = Some(notify);
        self.notify();
    }
    pub fn request(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired {
            return false;
        }
        s.requests += 1;
        // The engine must re-authorize eviction after applying this request.
        s.idle = false;
        s.touched = Instant::now();
        true
    }
    pub fn finish_request(&self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .requests -= 1;
        self.notify();
    }
    pub fn write(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired {
            return false;
        }
        s.writes += 1;
        true
    }
    pub fn finish_write(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).writes -= 1;
        self.notify();
    }
    pub fn set_idle(&self, idle: bool) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).idle = idle;
    }
    pub fn set_input_state(&self, focused: bool, composing: bool) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.input = true;
        s.focused = focused;
        s.composing = composing;
        s.idle = !focused && !composing;
    }
    pub fn candidate(&self) -> Option<(u8, Instant)> {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !s.input || s.retired || s.requests != 0 || s.writes != 0 {
            return None;
        }
        Some((
            if s.focused {
                2
            } else if s.composing {
                1
            } else {
                0
            },
            s.touched,
        ))
    }
    pub fn reclaim(&self, expected: (u8, Instant), close: impl FnOnce()) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let rank = if s.focused {
            2
        } else if s.composing {
            1
        } else {
            0
        };
        if !s.input
            || s.retired
            || s.requests != 0
            || s.writes != 0
            || (rank, s.touched) != expected
        {
            return false;
        }
        s.retired = true;
        close();
        true
    }
    pub fn stale_since(&self, age: Duration) -> Option<Instant> {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        (!s.retired && s.idle && s.requests == 0 && s.writes == 0 && s.touched.elapsed() >= age)
            .then_some(s.touched)
    }
    pub fn evict(&self, age: Duration, close: impl FnOnce()) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.retired || !s.idle || s.requests != 0 || s.writes != 0 || s.touched.elapsed() < age {
            return false;
        }
        s.retired = true;
        close();
        true
    }
}

/// Holds admission protection until a received request finishes engine processing.
pub struct RequestLease(pub(super) Arc<Activity>);
impl Drop for RequestLease {
    fn drop(&mut self) {
        self.0.finish_request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_reclaim_ranks_composition_below_focus_and_never_interrupts_requests() {
        let activity = Activity::default();
        assert!(activity.candidate().is_none()); // control/unknown peer
        activity.set_input_state(true, true);
        assert_eq!(activity.candidate().unwrap().0, 2);
        activity.set_input_state(false, true);
        assert_eq!(activity.candidate().unwrap().0, 1);
        activity.set_input_state(false, false);
        let old = activity.candidate().unwrap();
        assert_eq!(old.0, 0);
        activity.request();
        assert!(activity.candidate().is_none());
        assert!(!activity.reclaim(old, || panic!("in-flight request retired")));
        activity.write();
        activity.finish_request();
        assert!(activity.candidate().is_none());
        activity.finish_write();
        activity.set_input_state(true, true);
        assert!(!activity.reclaim(old, || panic!("obsolete rank retired")));
        assert!(activity.reclaim(activity.candidate().unwrap(), || {}));
        assert!(!activity.request());
    }
    #[test]
    fn eviction_requires_idle_no_requests_no_writes_and_minimum_age() {
        let activity = Activity::default();
        assert!(activity.stale_since(Duration::ZERO).is_none());
        activity.set_idle(true);
        assert!(activity.stale_since(Duration::from_secs(30)).is_none());
        activity.request();
        assert!(!activity.evict(Duration::ZERO, || panic!("busy request evicted")));
        activity.write();
        activity.finish_request();
        assert!(!activity.evict(Duration::ZERO, || panic!("pending write evicted")));
        activity.finish_write();
        activity.set_idle(true);
        assert!(activity.evict(Duration::ZERO, || {}));
        assert!(!activity.request());
        assert!(!activity.write());
        activity.set_idle(false);
        assert!(!activity.evict(Duration::ZERO, || panic!("active connection evicted")));
    }
    #[test]
    fn dropped_request_wakes_even_without_a_queue_slot() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let activity = Arc::new(Activity::default());
        activity.set_notifier(Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
        }));
        activity.request();
        let before = wakes.load(Ordering::Relaxed);
        drop(RequestLease(activity));
        assert_eq!(wakes.load(Ordering::Relaxed), before + 1);
    }
}
