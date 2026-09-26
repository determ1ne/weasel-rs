//! 通过宿主 TSF 线程管理器的私有 compartment 保存已确认的输入模式。
//!
//! 该值只表达输入模式，不保存连接或组合状态；服务停用或析构时不清除此宿主值，
//! 也不使用跨进程全局变量。
use super::*;
use crate::bindings::{GUID_WEASEL_INPUT_MODE, ITfCompartmentMgr, VariantClear};

/// 读取私有 compartment；仅接受 `VT_I4` 的 `0`/`1`，其他值视为未知。
fn read(manager: &ITfThreadMgr) -> Result<Option<bool>> {
    let compartments: ITfCompartmentMgr = manager.cast()?;
    let compartment = unsafe { compartments.GetCompartment(&GUID_WEASEL_INPUT_MODE)? };
    let mut value = unsafe { compartment.GetValue()? };
    let mode = unsafe {
        let inner = &value.Anonymous.Anonymous;
        if inner.vt == VARTYPE(VT_I4 as u16) {
            match inner.Anonymous.lVal {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            }
        } else {
            None
        }
    };
    unsafe {
        let _ = VariantClear(&mut value);
    }
    Ok(mode)
}

/// 把 ASCII 模式写入宿主线程管理器的私有 compartment。
///
/// `tid` 必须是当前服务向 TSF 注册时取得的客户端 ID；传给 COM 的 VARIANT 按 ABI 布局构造。
fn write(manager: &ITfThreadMgr, tid: TfClientId, ascii: bool) -> Result<()> {
    let compartments: ITfCompartmentMgr = manager.cast()?;
    let compartment = unsafe { compartments.GetCompartment(&GUID_WEASEL_INPUT_MODE)? };
    let value = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VARTYPE(VT_I4 as u16),
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 {
                    lVal: i32::from(ascii),
                },
            }),
        },
    };
    unsafe { compartment.SetValue(tid, &value).ok() }
}

impl TextService {
    /// 激活时读取宿主保存的输入模式，并同步到 RPC 工作线程。
    ///
    /// TSF 读取失败只报告诊断并记为未知，不阻止文本服务继续工作。
    pub(super) fn load_input_mode(&self) -> Result<()> {
        let manager = self.lock(&self.thread_mgr)?.clone();
        let mode = match manager.as_ref().map(read).transpose() {
            Ok(mode) => mode.flatten(),
            Err(error) => {
                crate::rpc_diagnostics::report("input-mode-read", None, error);
                None
            }
        };
        self.lock(&self.rpc)?.remember_mode(mode);
        Ok(())
    }

    /// 记住引擎确认的新模式，并尽力持久化到宿主 compartment。
    ///
    /// 调用 TSF 前先释放服务状态锁，以免宿主回调重入时发生锁死；写入失败不应禁用输入。
    pub(super) fn remember_input_mode(&self, ascii: bool) -> Result<()> {
        if self.lock(&self.rpc)?.remembered_mode() == Some(ascii) {
            return Ok(());
        }
        // No service locks are held while calling into TSF.
        let manager = self.lock(&self.thread_mgr)?.clone();
        let tid = *self.lock(&self.keystroke_client_id)?;
        if let (Some(manager), Some(tid)) = (manager, tid) {
            if let Err(error) = write(&manager, tid, ascii) {
                // Remembering a preference must never disable host input.
                crate::rpc_diagnostics::report("input-mode-write", None, error);
            }
        }
        self.lock(&self.rpc)?.remember_mode(Some(ascii));
        Ok(())
    }
}
