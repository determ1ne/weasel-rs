use super::*;
use crate::bindings::TF_ES_ASYNC;

impl TextService {
    /// Called after a successful composition write. A host need not emit a
    /// layout notification for every edit; request a read after the write lock
    /// is released instead of relying on OnLayoutChange for the first anchor.
    pub(super) fn request_composition_layout(&self, state: &Arc<ContextState>) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || self.faulted.load(Ordering::Acquire)
            || *self.lock(&self.focused_context)? != Some(state.id)
        {
            return Ok(());
        }
        let Some(composition) = self.lock(&state.composition)?.as_ref().cloned() else {
            return Ok(());
        };
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        let view = unsafe { state.context.GetActiveView()? };
        let range = unsafe { composition.GetRange()? };
        let probe: ITfEditSession = LayoutProbe {
            view,
            range,
            composition,
            state: state.clone(),
            token: state.token()?,
            generation: Arc::clone(&self.generation),
            requested_generation: self.generation.load(Ordering::Acquire),
            _module: ModuleLease::new(),
        }
        .into();
        // Never synchronously reenter an edit session while updating preedit.
        let result = unsafe {
            state
                .context
                .RequestEditSession(tid, &probe, TF_ES_ASYNC | TF_ES_READ)
        };
        result?.ok()
    }
}

#[implement(ITfEditSession)]
pub(super) struct LayoutProbe {
    pub(super) view: ITfContextView,
    pub(super) range: ITfRange,
    pub(super) composition: ITfComposition,
    pub(super) state: Arc<ContextState>,
    pub(super) token: weasel_common::message::ContextToken,
    pub(super) generation: Arc<AtomicU64>,
    pub(super) requested_generation: u64,
    pub(super) _module: ModuleLease,
}

impl ITfEditSession_Impl for LayoutProbe_Impl {
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            if !self.state.matches(Some(&self.token))? {
                return Ok(());
            }
            if self.generation.load(Ordering::Acquire) != self.requested_generation {
                return Ok(());
            }
            if self
                .state
                .composition
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .as_ref()
                != Some(&self.composition)
            {
                return Ok(());
            }
            let mut rect = RECT::default();
            let mut clipped = BOOL(0);
            let hr = unsafe {
                self.view
                    .GetTextExt(ec, &self.range, &mut rect, &mut clipped)
            };
            if hr.is_err() {
                self.state
                    .rpc
                    .try_lock()
                    .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                    .log(
                        "debug",
                        format!("layout anchor GetTextExt failed hr={hr:?}"),
                    );
                return Ok(());
            }
            if self.generation.load(Ordering::Acquire) != self.requested_generation {
                return Ok(());
            }
            if !self.state.matches(Some(&self.token))?
                || self
                    .state
                    .composition
                    .try_lock()
                    .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                    .as_ref()
                    != Some(&self.composition)
            {
                return Ok(());
            }
            self.state
                .rpc
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .send_layout_update(LayoutUpdate {
                    session_id: self.state.id,
                    token: Some(self.token.clone()),
                    anchor: Some(RenderRect {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                        valid: rect.right > rect.left && rect.bottom > rect.top,
                    }),
                });
            Ok(())
        })
    }
}
