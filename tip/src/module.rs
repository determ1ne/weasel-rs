//! DLL lifetime accounting, independent of any particular COM interface.
use std::sync::atomic::{AtomicUsize, Ordering};

static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static SERVER_LOCKS: AtomicUsize = AtomicUsize::new(0);

/// Keep this as the LAST field so dependencies are released before the lease.
pub(crate) struct ModuleLease;

impl ModuleLease {
    pub(crate) fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ModuleLease {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn lock_server(lock: bool) {
    if lock {
        SERVER_LOCKS.fetch_add(1, Ordering::AcqRel);
    } else {
        let _ =
            SERVER_LOCKS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }
}

pub(crate) fn can_unload() -> bool {
    OBJECTS.load(Ordering::Acquire) == 0 && SERVER_LOCKS.load(Ordering::Acquire) == 0
}
