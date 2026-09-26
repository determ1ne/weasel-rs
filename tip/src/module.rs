//! 汇总 COM 对象、辅助线程和服务器锁对 DLL 存活期的约束。
//!
//! COM 卸载查询只在所有租约及显式服务器锁归零时允许卸载；计数采用原子操作，避免依赖
//! 某个具体接口的析构顺序或跨线程锁。
use std::sync::atomic::{AtomicUsize, Ordering};

static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static SERVER_LOCKS: AtomicUsize = AtomicUsize::new(0);

/// 保持 DLL 加载的 RAII 租约。
///
/// 若作为结构体字段，必须放在最后，使其他字段先释放、租约最后释放，确保其析构所需代码
/// 和依赖仍处于已加载状态。
pub(crate) struct ModuleLease;

impl ModuleLease {
    /// 创建租约并增加全局对象计数。
    pub(crate) fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ModuleLease {
    /// 释放租约并减少全局对象计数。
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 更新 COM `IClassFactory::LockServer` 对应的服务器锁计数。
///
/// 解锁采用饱和式失败处理，不会在计数已为零时发生下溢。
pub(crate) fn lock_server(lock: bool) {
    if lock {
        SERVER_LOCKS.fetch_add(1, Ordering::AcqRel);
    } else {
        let _ =
            SERVER_LOCKS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }
}

/// 判断 COM 是否可以卸载 DLL，并顺手移除已完成的诊断线程租约。
pub(crate) fn can_unload() -> bool {
    crate::diagnostics::reap_finished();
    OBJECTS.load(Ordering::Acquire) == 0 && SERVER_LOCKS.load(Ordering::Acquire) == 0
}
