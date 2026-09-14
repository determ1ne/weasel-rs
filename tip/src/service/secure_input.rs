//! Detect password input scopes under a TSF read lock and keep the result on
//! the document context. Unknown state fails closed until a probe succeeds.
use super::*;
use crate::bindings::{
    CoTaskMemFree, GUID_PROP_INPUTSCOPE, IS_NUMERIC_PASSWORD, IS_PASSWORD, InputScopeManual,
    TF_DEFAULT_SELECTION, TF_ES_SYNC, VariantClear,
};
use weasel_common::message::ContextAction;

pub(super) const UNKNOWN: u8 = 0;
const NORMAL: u8 = 1;
const SECURE: u8 = 2;
const MAX_INPUT_SCOPES: u32 = 64;

#[derive(Default)]
pub(super) struct SecureProbeSchedule {
    serial: u64,
    pending: Option<u64>,
}

impl SecureProbeSchedule {
    fn request(&mut self, replace: bool) -> Option<u64> {
        if self.pending.is_some() && !replace {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some(self.serial);
        Some(self.serial)
    }

    fn take(&mut self, ticket: u64) -> bool {
        if self.pending == Some(ticket) {
            self.pending = None;
            true
        } else {
            false
        }
    }
}

struct ScopeBuffer(*mut InputScopeManual);

impl Drop for ScopeBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }
}

fn selected_range(context: &ITfContext, ec: TfEditCookie) -> Result<Option<ITfRange>> {
    let mut selection = TF_SELECTION {
        range: std::mem::ManuallyDrop::new(None),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: BOOL(0),
        },
    };
    let mut fetched = 0;
    let result =
        unsafe { context.GetSelection(ec, TF_DEFAULT_SELECTION, 1, &mut selection, &mut fetched) };
    let range = unsafe { std::mem::ManuallyDrop::take(&mut selection.range) };
    result.ok()?;
    Ok((fetched == 1).then_some(range).flatten())
}

fn input_scope_unknown(value: &mut VARIANT) -> Option<IUnknown> {
    unsafe {
        let inner = &*value.Anonymous.Anonymous;
        if inner.vt != VARTYPE(VT_UNKNOWN as u16) {
            return None;
        }
        // Keep our own COM reference and let VariantClear release the one
        // owned by the VARIANT; never mutate an active union member by hand.
        (&*inner.Anonymous.punkVal).clone()
    }
}

fn query_secure_field(context: &ITfContext, ec: TfEditCookie) -> Result<bool> {
    let Some(range) = selected_range(context, ec)? else {
        return Ok(false);
    };
    // Missing app properties are normal for ordinary Win32 text stores.
    let Ok(property): Result<ITfReadOnlyProperty> =
        (unsafe { context.GetAppProperty(&GUID_PROP_INPUTSCOPE) })
    else {
        return Ok(false);
    };
    let Ok(mut value) = (unsafe { property.GetValue(ec, &range) }) else {
        return Ok(false);
    };
    let unknown = input_scope_unknown(&mut value);
    unsafe {
        let _ = VariantClear(&mut value);
    }
    let Some(unknown) = unknown else {
        return Ok(false);
    };
    let Ok(input_scope) = unknown.cast::<ITfInputScope>() else {
        return Ok(false);
    };
    let mut scopes = std::ptr::null_mut();
    let mut count = 0;
    unsafe { input_scope.GetInputScopes(&mut scopes, &mut count).ok()? };
    let scopes = ScopeBuffer(scopes);
    if count > MAX_INPUT_SCOPES || (count != 0 && scopes.0.is_null()) {
        return Err(Error::from_hresult(boundary::E_FAIL));
    }
    let values = if count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(scopes.0, count as usize) }
    };
    Ok(values
        .iter()
        .any(|scope| matches!(*scope, IS_PASSWORD | IS_NUMERIC_PASSWORD)))
}

#[implement(ITfEditSession)]
struct SecureFieldProbe {
    service: *const TextService,
    state: Arc<ContextState>,
    ticket: u64,
    generation: u64,
    _owner: IUnknown,
    _module: ModuleLease,
}

impl Drop for SecureFieldProbe {
    fn drop(&mut self) {
        if let Ok(mut schedule) = self.state.secure_probe.try_lock() {
            schedule.take(self.ticket);
        }
    }
}

impl ITfEditSession_Impl for SecureFieldProbe_Impl {
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            let requested = self
                .state
                .secure_probe
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .take(self.ticket);
            if !requested
                || !self.state.alive.load(Ordering::Acquire)
                || self.state.generation.load(Ordering::Acquire) != self.generation
            {
                return Ok(());
            }
            let secure = query_secure_field(&self.state.context, ec)?;
            let service = unsafe { &*self.service };
            service.set_secure_field(&self.state, secure);
            Ok(())
        })
    }
}

impl TextService {
    pub(super) fn request_secure_field_probe(
        &self,
        state: &Arc<ContextState>,
        owner: IUnknown,
        synchronous: bool,
    ) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || !state.alive.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        // A key callback must not wait for an older asynchronous focus probe:
        // supersede it with a synchronous read and let the old callback no-op.
        let Some(ticket) = self.lock(&state.secure_probe)?.request(synchronous) else {
            return Ok(());
        };
        let probe: ITfEditSession = SecureFieldProbe {
            service: self,
            state: state.clone(),
            ticket,
            generation: state.generation.load(Ordering::Acquire),
            _owner: owner,
            _module: ModuleLease::new(),
        }
        .into();
        let flags = if synchronous {
            TF_ES_SYNC | TF_ES_READ
        } else {
            TF_ES_ASYNCDONTCARE | TF_ES_READ
        };
        let result = unsafe { state.context.RequestEditSession(tid, &probe, flags) }
            .and_then(|session| session.ok());
        if result.is_err() {
            self.lock(&state.secure_probe)?.take(ticket);
        }
        result
    }

    pub(super) fn refresh_secure_field_from_cookie(&self, state: &ContextState, ec: TfEditCookie) {
        if let Ok(secure) = query_secure_field(&state.context, ec) {
            self.set_secure_field(state, secure);
        }
    }

    fn set_secure_field(&self, state: &ContextState, secure: bool) {
        let next = if secure { SECURE } else { NORMAL };
        if state.secure_field.swap(next, Ordering::AcqRel) == next {
            return;
        }
        self.wake_for_secure_update();
    }

    fn wake_for_secure_update(&self) {
        let hwnd = self
            .update_window
            .try_lock()
            .ok()
            .and_then(|window| window.as_ref().map(|window| window.hwnd));
        if let Some(hwnd) = hwnd {
            unsafe {
                let _ = bindings::PostMessageW(
                    Some(hwnd),
                    update_window::UPDATE_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    pub(super) fn secure_field_visible(&self, state: &ContextState) -> bool {
        self.secure_mode.load(Ordering::Acquire)
            || state.secure_field.load(Ordering::Acquire) == SECURE
    }

    pub(super) fn allow_rime_for_secure_field(&self, state: &ContextState) -> Result<bool> {
        let epoch = self.lock(&state.rpc)?.connection_epoch();
        Ok(epoch != 0
            && self.secure_policy_epoch.load(Ordering::Acquire) == epoch
            && self.allow_rime_in_secure_fields.load(Ordering::Acquire))
    }

    pub(super) fn should_bypass_secure_field(&self, state: &ContextState) -> Result<bool> {
        Ok(!self.allow_rime_for_secure_field(state)?
            && (self.secure_mode.load(Ordering::Acquire)
                || state.secure_field.load(Ordering::Acquire) != NORMAL))
    }

    pub(super) fn reconcile_secure_field(&self) -> Result<()> {
        let focused = *self.lock(&self.focused_context)?;
        let state = self
            .lock(&self.contexts)?
            .iter()
            .find(|state| Some(state.id) == focused)
            .cloned();
        let Some(state) = state else {
            return Ok(());
        };
        // Unknown is fail-closed only at the key boundary. Do not invalidate
        // the context while its asynchronous focus probe is still pending.
        let secure = self.secure_field_visible(&state);
        let bypass = secure && !self.allow_rime_for_secure_field(&state)?;
        if !bypass {
            state.secure_bypass_active.store(false, Ordering::Release);
            return Ok(());
        }
        self.lock(&self.tested_key)?.take();
        if state.secure_bypass_active.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let connected = state.token()?.connection_epoch != 0;
        let has_composition = self.lock(&state.composition)?.is_some();
        if connected || has_composition {
            self.send_context_action(&state, ContextAction::Cancel, true)?;
        }
        Ok(())
    }

    pub(super) fn update_secure_policy(&self, allow: Option<bool>, epoch: u64) {
        let allow = allow.unwrap_or(false);
        let changed = self
            .allow_rime_in_secure_fields
            .swap(allow, Ordering::AcqRel)
            != allow;
        let epoch_changed = self.secure_policy_epoch.swap(epoch, Ordering::AcqRel) != epoch;
        if changed || epoch_changed {
            self.wake_for_secure_update();
        }
    }
}
