use super::*;

#[test]
fn retirement_never_interrupts_requests_or_writes() {
    let activity = Activity::default();
    let old = activity.candidate().unwrap();

    activity.request();
    assert!(activity.candidate().is_none());
    assert!(!activity.retire(old, || panic!("in-flight request retired")));
    activity.write();
    activity.finish_request();
    assert!(activity.candidate().is_none());
    activity.finish_write();

    assert!(!activity.retire(old, || panic!("obsolete candidate retired")));
    assert!(activity.retire(activity.candidate().unwrap(), || {}));
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
