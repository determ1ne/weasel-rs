//! Remember only the confirmed mode, not connections or compositions.
//! The private compartment belongs to the host's TSF thread manager and is
//! intentionally not cleared by Deactivate or Drop. No cross-process globals.
use super::*;
use crate::bindings::{GUID_WEASEL_INPUT_MODE, ITfCompartmentMgr, VariantClear};

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
