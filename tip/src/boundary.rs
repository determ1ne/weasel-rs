//! Last-resort containment at our Rust/COM and window-procedure entry points.
//! Expected failures remain Results. A panicked service must not resume editing.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard, atomic::Ordering};
use windows_core::{Error, Result};

pub(crate) use crate::bindings::E_FAIL;

#[track_caller]
pub(crate) fn guard<T>(
    faulted: Option<&crate::diagnostics::FaultState>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if faulted.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(Error::from_hresult(E_FAIL));
    }
    if let Some(flag) = faulted {
        flag.event("callback.enter", 0);
    }
    let result = match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(payload) => {
            if let Some(flag) = faulted {
                flag.mark("panic", 0);
            }
            // A foreign panic payload can itself panic during Drop.
            std::mem::forget(payload);
            Err(Error::from_hresult(E_FAIL))
        }
    };
    if let Some(flag) = faulted {
        flag.event(
            "callback.exit",
            result
                .as_ref()
                .err()
                .map_or(0, |e| e.code().0 as u32 as u64),
        );
    }
    result
}

/// Poison recovery is ONLY for dismantling state, never for resuming input.
pub(crate) fn teardown_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn cleanup(operation: impl FnOnce()) {
    let _ = guard(None, || {
        operation();
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::FaultState as AtomicBool;

    #[test]
    fn panic_disables_subsequent_operations() {
        let faulted = AtomicBool::new(false);
        let result: Result<()> = guard(Some(&faulted), || panic!("injected"));
        assert!(result.is_err());
        assert!(guard::<()>(Some(&faulted), || panic!("must not execute")).is_err());
    }

    #[test]
    fn ordinary_error_does_not_poison_service() {
        let faulted = AtomicBool::new(false);
        let result: Result<()> = guard(Some(&faulted), || Err(Error::from_hresult(E_FAIL)));
        assert!(result.is_err());
        assert!(!faulted.load(Ordering::Acquire));
    }
}
