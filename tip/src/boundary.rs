//! Last-resort containment at our Rust/COM and window-procedure entry points.
//! Expected failures remain Results. A panicked service must not resume editing.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use windows_core::{Error, Result};

pub(crate) use crate::bindings::E_FAIL;

pub(crate) fn guard<T>(
    faulted: Option<&AtomicBool>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if faulted.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(Error::from_hresult(E_FAIL));
    }
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(payload) => {
            if let Some(flag) = faulted {
                flag.store(true, Ordering::Release);
            }
            // A foreign panic payload can itself panic during Drop.
            std::mem::forget(payload);
            Err(Error::from_hresult(E_FAIL))
        }
    }
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
