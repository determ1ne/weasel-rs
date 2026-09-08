//! TSF apartment state. COM entry points delegate to responsibility-specific modules.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
mod callbacks;
mod composition;
mod context;
mod host_edit;
use context::ContextState;
mod display_attribute;
mod edit_session;
mod input_mode;
mod key_event;
mod language_bar;
mod layout;
mod lifecycle;
mod range;
mod response;
mod safety;
use display_attribute::{DisplayAttributeEnumerator, DisplayAttributeInfo};

use std::{
    collections::VecDeque,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

use crate::bindings::{
    CLSCTX_INPROC_SERVER, CLSID_TF_CategoryMgr, E_NOINTERFACE, E_NOTIMPL, E_POINTER,
    GUID_PROP_ATTRIBUTE, GUID_WEASEL_DISPLAY_ATTRIBUTE, IEnumTfDisplayAttributeInfo,
    IEnumTfDisplayAttributeInfo_Impl, ITfActiveLanguageProfileNotifySink,
    ITfActiveLanguageProfileNotifySink_Impl, ITfCategoryMgr, ITfCompartmentEventSink,
    ITfCompartmentEventSink_Impl, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextComposition, ITfContextView, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl,
    ITfDocumentMgr, ITfEditRecord, ITfEditSession, ITfEditSession_Impl, ITfInsertAtSelection,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfProperty, ITfRange, ITfSource,
    ITfTextEditSink, ITfTextEditSink_Impl, ITfTextInputProcessor, ITfTextInputProcessor_Impl,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextLayoutSink,
    ITfTextLayoutSink_Impl, ITfThreadFocusSink, ITfThreadFocusSink_Impl, ITfThreadMgr,
    ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, LPARAM, RECT, S_FALSE, TF_AE_NONE,
    TF_ANCHOR_END, TF_ANCHOR_START, TF_ATTR_INPUT, TF_CT_NONE, TF_DA_COLOR, TF_DA_COLOR_0,
    TF_DISPLAYATTRIBUTE, TF_ES_ASYNCDONTCARE, TF_ES_READ, TF_ES_READWRITE, TF_IAS_QUERYONLY,
    TF_LS_DOT, TF_SELECTION, TF_SELECTIONSTYLE, TfClientId, TfEditCookie, TfGuidAtom, TfLayoutCode,
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VARTYPE, VT_I4, WPARAM,
};
use crate::rpc_worker::RpcWorker;
use crate::{bindings, boundary, update_window};
use weasel_common::message::{KeyEventResponse, LayoutUpdate, RenderRect};
use windows_core::{BOOL, Error, GUID, IUnknown, IUnknownImpl, Interface, Ref, Result, implement};

use crate::module::ModuleLease;

#[derive(Clone, Copy)]
enum EditStep {
    ApplyResponse,
    StartComposition,
    UpdateComposition,
    CommitComposition,
    InsertCommit,
    DisconnectComposition,
    EndComposition { clear: bool, restart: bool },
}

impl EditStep {
    fn name(self) -> &'static str {
        match self {
            Self::ApplyResponse => "ApplyResponse",
            Self::StartComposition => "StartComposition",
            Self::UpdateComposition => "UpdateComposition",
            Self::CommitComposition => "CommitComposition",
            Self::InsertCommit => "InsertCommit",
            Self::DisconnectComposition => "DisconnectComposition",
            Self::EndComposition { .. } => "EndComposition",
        }
    }
}

struct PendingEdit {
    state: Arc<ContextState>,
    context: ITfContext,
    response: KeyEventResponse,
    step: EditStep,
    session: IUnknown,
}

fn not_implemented<T>() -> Result<T> {
    Err(Error::from_hresult(E_NOTIMPL))
}

impl PendingEdit {
    fn matches(&self) -> Result<bool> {
        if matches!(self.step, EditStep::DisconnectComposition) {
            return Ok(self.state.alive.load(Ordering::Acquire)
                && !self.state.suspended.load(Ordering::Acquire)
                && self.response.token.as_ref().is_some_and(|t| {
                    t.generation == self.state.generation.load(Ordering::Acquire)
                }));
        }
        self.state.matches(self.response.token.as_ref())
    }
}

#[implement(
    ITfTextInputProcessor,
    ITfTextInputProcessorEx,
    ITfThreadMgrEventSink,
    ITfTextEditSink,
    ITfTextLayoutSink,
    ITfKeyEventSink,
    ITfThreadFocusSink,
    ITfCompositionSink,
    ITfActiveLanguageProfileNotifySink,
    ITfDisplayAttributeProvider,
    ITfCompartmentEventSink
)]
pub(crate) struct TextService {
    contexts: Mutex<Vec<Arc<ContextState>>>,
    focused_context: Mutex<Option<u64>>,
    tested_key: Mutex<Option<key_event::TestedKey>>,
    activated: AtomicBool,
    teardown_pending: AtomicBool,
    tearing_down: AtomicBool,
    edit_mutated: AtomicBool,
    faulted: crate::diagnostics::FaultState,
    generation: Arc<AtomicU64>,
    rpc: Arc<Mutex<RpcWorker>>,
    update_window: Mutex<Option<update_window::UpdateWindow>>,
    language_bar: Mutex<Option<Arc<language_bar::LanguageBar>>>,
    thread_mgr: Mutex<Option<ITfThreadMgr>>,
    thread_mgr_event_sink_cookie: Mutex<Option<u32>>,
    thread_focus_sink_cookie: Mutex<Option<u32>>,
    keystroke_mgr: Mutex<Option<ITfKeystrokeMgr>>,
    keystroke_client_id: Mutex<Option<TfClientId>>,
    pending_edit: Mutex<VecDeque<PendingEdit>>,
    edit_requested: AtomicBool,
    edit_ticket: AtomicU64,
    active_edit_context: AtomicU64,
    display_attribute_atom: Mutex<Option<TfGuidAtom>>,
    _module: ModuleLease,
}

impl TextService {
    #[track_caller]
    fn lock<'a, T>(
        &'a self,
        state: &'a Mutex<T>,
    ) -> Result<crate::diagnostics::TrackedGuard<'a, T>> {
        // Apartment state must never block a reentrant COM callback.
        let address = state as *const Mutex<T> as usize as u64;
        self.faulted.event("lock.try", address);
        match state.try_lock() {
            Ok(guard) => Ok(self.faulted.track(guard, address)),
            Err(error) => {
                let reason = match error {
                    std::sync::TryLockError::WouldBlock => {
                        self.faulted.event("lock.would_block", address);
                        self.faulted.request_maintenance();
                        return Err(Error::from_hresult(boundary::E_PENDING));
                    }
                    std::sync::TryLockError::Poisoned(_) => "lock.poisoned",
                };
                self.faulted.mark(reason, address);
                Err(Error::from_hresult(boundary::E_FAIL))
            }
        }
    }

    pub(crate) fn new() -> Self {
        Self {
            contexts: Mutex::new(Vec::new()),
            focused_context: Mutex::new(None),
            tested_key: Mutex::new(None),
            activated: AtomicBool::new(false),
            teardown_pending: AtomicBool::new(false),
            tearing_down: AtomicBool::new(false),
            edit_mutated: AtomicBool::new(false),
            faulted: crate::diagnostics::FaultState::new(false),
            generation: Arc::new(AtomicU64::new(0)),
            rpc: Arc::new(Mutex::new(RpcWorker::default())),
            update_window: Mutex::new(None),
            language_bar: Mutex::new(None),
            thread_mgr: Mutex::new(None),
            thread_mgr_event_sink_cookie: Mutex::new(None),
            thread_focus_sink_cookie: Mutex::new(None),
            keystroke_mgr: Mutex::new(None),
            keystroke_client_id: Mutex::new(None),
            pending_edit: Mutex::new(VecDeque::new()),
            edit_requested: AtomicBool::new(false),
            edit_ticket: AtomicU64::new(0),
            active_edit_context: AtomicU64::new(0),
            display_attribute_atom: Mutex::new(None),
            _module: ModuleLease::new(),
        }
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        self.deactivate_internal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn poisoned_state_is_contained_at_com_boundary_and_drop_is_safe() {
        let service = windows_core::ComObject::new(TextService::new());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _lock = service.tested_key.lock().unwrap();
            panic!("injected poisoned input cache");
        }));
        let keys: ITfKeyEventSink = service.to_interface();
        assert!(unsafe { keys.OnSetFocus(true) }.is_err());
        assert!(service.faulted.load(Ordering::Acquire));
        assert!(
            !unsafe { keys.OnTestKeyDown(None, WPARAM(65), LPARAM(0)) }
                .unwrap()
                .as_bool()
        );
        let processor: ITfTextInputProcessor = service.to_interface();
        unsafe {
            processor.Deactivate().unwrap();
            processor.Deactivate().unwrap();
        }
        // Dropping all COM references also exercises no-panic teardown.
    }

    #[test]
    fn reentrant_state_access_fails_without_waiting() {
        let service = TextService::new();
        let guard = service.tested_key.lock().unwrap();
        assert_eq!(
            service.lock(&service.tested_key).err().unwrap().code(),
            boundary::E_PENDING
        );
        assert!(!service.faulted.load(Ordering::Acquire));
        drop(guard);
        assert!(service.lock(&service.tested_key).is_ok());
    }

    #[test]
    fn teardown_busy_state_is_retained_until_retry() {
        let service = TextService::new();
        service.activated.store(true, Ordering::Release);
        let guard = service.tested_key.lock().unwrap();
        service.deactivate_internal();
        assert!(service.teardown_pending.load(Ordering::Acquire));
        assert!(!service.activated.load(Ordering::Acquire));
        assert!(!service.faulted.load(Ordering::Acquire));
        drop(guard);
        service.deactivate_internal();
        assert!(!service.teardown_pending.load(Ordering::Acquire));
    }
}
