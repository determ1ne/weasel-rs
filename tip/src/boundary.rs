//! Rust 与 COM、Win32 窗口过程交界处的最后一道故障隔离。
//!
//! 可预期失败仍通过 `Result` 返回；panic 被限制在 Rust 内，并可将 TIP 实例置为故障态，
//! 之后不再继续编辑。此模块不替代各调用点对线程、锁和对象生命周期的约束。
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard, atomic::Ordering};
use windows_core::{Error, Result};

pub(crate) use crate::bindings::E_FAIL;
pub(crate) use crate::bindings::E_PENDING;

/// 非阻塞地取得拆除期间所需的互斥锁。
///
/// 中毒锁只允许取出状态用于释放资源；若锁正被使用则返回 `None`，调用方须保留对象，
/// 等待之后的单元线程回调再拆除，不能在此等待或跨 apartment 操作。
pub(crate) fn try_teardown<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// 在可选实例故障门闩保护下执行可能失败或 panic 的操作。
///
/// 已故障实例直接返回 `E_FAIL`；新 panic 被捕获并标记故障，异常负载故意泄漏，以免其
/// 析构再次 panic。该函数把失败转为 HRESULT 所需的 `Error`，调用方负责最终 ABI 映射。
#[track_caller]
pub(crate) fn guard<T>(
    faulted: Option<&crate::diagnostics::FaultState>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if faulted.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(Error::from_hresult(E_FAIL));
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
    result
}

/// 尽力执行不返回结果的清理操作，并隔离其 panic。
///
/// 清理阶段不改变实例故障标志，也不允许异常越过析构或外部回调边界。
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
