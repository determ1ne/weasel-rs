use super::*;
use crate::bindings::{
    GA_ROOT, GetAncestor, LogicalToPhysicalPointForPerMonitorDPI, POINT, TF_ES_ASYNC,
};
use weasel_common::message::ContextToken;

/// One pending TSF read, refreshed by notifications received before it runs.
#[derive(Default)]
pub(super) struct LayoutSchedule {
    serial: u64,
    pending: Option<(u64, ContextToken)>,
}
impl LayoutSchedule {
    fn request(&mut self, token: ContextToken) -> Option<u64> {
        if let Some((_, latest)) = &mut self.pending {
            *latest = token;
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some((self.serial, token));
        Some(self.serial)
    }
    fn take(&mut self, ticket: u64) -> Option<ContextToken> {
        if self.pending.as_ref().is_some_and(|(id, _)| *id == ticket) {
            self.pending.take().map(|(_, token)| token)
        } else {
            None
        }
    }
}
impl TextService {
    pub(super) fn request_composition_layout(&self, state: &Arc<ContextState>) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
            || self.faulted.load(Ordering::Acquire)
            || *self.lock(&self.focused_context)? != Some(state.id)
            || self.lock(&state.composition)?.is_none()
        {
            return Ok(());
        }
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        let token = state.token()?;
        let Some(ticket) = self.lock(&state.layout)?.request(token) else {
            return Ok(());
        };
        let probe: ITfEditSession = LayoutProbe {
            state: state.clone(),
            ticket,
            generation: Arc::clone(&self.generation),
            requested_generation: self.generation.load(Ordering::Acquire),
            _module: ModuleLease::new(),
        }
        .into();
        // Defer until the write lock is released; retain Terminal's explicit
        // post-composition probe even when no layout callback is delivered.
        let result = unsafe {
            state
                .context
                .RequestEditSession(tid, &probe, TF_ES_ASYNC | TF_ES_READ)
        }
        .and_then(|hr| hr.ok());
        if result.is_err() {
            self.lock(&state.layout)?.take(ticket);
        }
        result
    }
}
#[implement(ITfEditSession)]
pub(super) struct LayoutProbe {
    state: Arc<ContextState>,
    ticket: u64,
    generation: Arc<AtomicU64>,
    requested_generation: u64,
    _module: ModuleLease,
}
impl Drop for LayoutProbe {
    fn drop(&mut self) {
        // TSF can discard an edit session without executing it.
        if let Ok(mut layout) = self.state.layout.try_lock() {
            layout.take(self.ticket);
        }
    }
}
impl ITfEditSession_Impl for LayoutProbe_Impl {
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            let Some(token) = self
                .state
                .layout
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .take(self.ticket)
            else {
                return Ok(());
            };
            if !self.state.matches(Some(&token))?
                || self.generation.load(Ordering::Acquire) != self.requested_generation
            {
                return Ok(());
            }
            let Some(composition) = self
                .state
                .composition
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .clone()
            else {
                return Ok(());
            };
            // Query current geometry, not a range captured before queued edits.
            let view = unsafe { self.state.context.GetActiveView()? };
            let range = unsafe { composition.GetRange()? };
            let mut rect = RECT::default();
            let mut clipped = BOOL(0);
            if unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }.is_err() {
                // Transient geometry failure must not flood the key/log queue.
                return Ok(());
            }
            let rect = physical_text_rect(&view, rect);
            if self.generation.load(Ordering::Acquire) != self.requested_generation
                || !self.state.matches(Some(&token))?
                || self
                    .state
                    .composition
                    .try_lock()
                    .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                    .as_ref()
                    != Some(&composition)
            {
                return Ok(());
            }
            self.state
                .rpc
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .send_layout_update(LayoutUpdate {
                    session_id: self.state.id,
                    token: Some(token),
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

/// The renderer uses physical screen pixels, whereas a text store may return
/// screen coordinates in its host window's DPI space. Like Mozc, use the root
/// window of the source view for the conversion, never the foreground window.
fn physical_text_rect(view: &ITfContextView, rect: RECT) -> RECT {
    unsafe {
        let Ok(window) = view.GetWnd() else {
            return rect;
        };
        if window.0.is_null() {
            return rect;
        }
        let root = GetAncestor(window, GA_ROOT as u32);
        if root.0.is_null() {
            return rect;
        }
        let mut start = POINT {
            x: rect.left,
            y: rect.top,
        };
        let mut end = POINT {
            x: rect.right,
            y: rect.bottom,
        };
        // Commit both corners together. Geometry conversion failure should not
        // interrupt typing or produce a rectangle mixing coordinate spaces.
        if !LogicalToPhysicalPointForPerMonitorDPI(Some(root), &mut start).as_bool()
            || !LogicalToPhysicalPointForPerMonitorDPI(Some(root), &mut end).as_bool()
        {
            return rect;
        }
        RECT {
            left: start.x,
            top: start.y,
            right: end.x,
            bottom: end.y,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalesces_and_old_drop_cannot_cancel_next_probe() {
        let mut schedule = LayoutSchedule::default();
        let first = schedule.request(ContextToken::default()).unwrap();
        let latest = ContextToken {
            generation: 2,
            ..Default::default()
        };
        for _ in 0..1000 {
            assert!(schedule.request(latest.clone()).is_none());
        }
        assert_eq!(schedule.take(first), Some(latest));
        let second = schedule.request(ContextToken::default()).unwrap();
        assert!(schedule.take(first).is_none());
        assert!(schedule.take(second).is_some());
    }
}
