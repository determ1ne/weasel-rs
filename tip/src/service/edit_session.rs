//! One TSF edit request owns one immutable task. A late callback cannot consume
//! a different request's context or edit cookie after reactivation.
use super::*;

#[implement(ITfEditSession)]
pub(super) struct ResponseEdit {
    service: *const TextService,
    pending: Mutex<Option<PendingEdit>>,
    generation: u64,
    ticket: u64,
    // Keeps `service` alive even after its pending task has been consumed.
    _owner: IUnknown,
    _module: ModuleLease,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_session_cannot_change_new_activations_scheduler() {
        let service = windows_core::ComObject::new(TextService::new());
        service.activated.store(true, Ordering::Release);
        service.generation.store(2, Ordering::Release);
        service.edit_requested.store(true, Ordering::Release);
        let session: ITfEditSession = ResponseEdit {
            service: service.get(),
            pending: Mutex::new(None),
            generation: 1,
            ticket: 0,
            _owner: service.to_interface(),
            _module: ModuleLease::new(),
        }
        .into();
        unsafe {
            session.DoEditSession(TfEditCookie(0)).unwrap();
        }
        assert!(service.edit_requested.load(Ordering::Acquire));
        assert!(!service.faulted.load(Ordering::Acquire));
    }
}

impl ResponseEdit {
    pub(super) fn new(
        service: &TextService,
        pending: PendingEdit,
        generation: u64,
        ticket: u64,
    ) -> Self {
        Self {
            service,
            _owner: pending.session.clone(),
            pending: Mutex::new(Some(pending)),
            generation,
            ticket,
            _module: ModuleLease::new(),
        }
    }
}

impl ITfEditSession_Impl for ResponseEdit_Impl {
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        // TSF dispatches this session on the requesting apartment; _owner keeps
        // the pointed-to COM allocation alive throughout this callback.
        let service = unsafe { &*self.service };
        let started = std::time::Instant::now();
        weasel_common::input_trace!(
            "edit.enter ticket={} activation={} cookie={:?}",
            self.ticket,
            self.generation,
            ec
        );
        let result = boundary::guard(Some(&service.faulted), || {
            if !service.activated.load(Ordering::Acquire)
                || service.generation.load(Ordering::Acquire) != self.generation
                || service.edit_ticket.load(Ordering::Acquire) != self.ticket
            {
                weasel_common::input_trace!(
                    "edit.skip ticket={} reason=stale_activation_or_ticket",
                    self.ticket
                );
                return Ok(());
            }
            let pending = service.lock(&self.pending)?.take();
            let Some(pending) = pending else {
                weasel_common::input_trace!(
                    "edit.skip ticket={} reason=already_consumed",
                    self.ticket
                );
                return Ok(());
            };
            let state = pending.state.clone();
            let valid = state.matches(pending.response.token.as_ref())?;
            weasel_common::input_trace!(
                "edit.apply ticket={} token={:?} revision={} step={} valid={}",
                self.ticket,
                pending.response.token,
                pending.response.revision,
                pending.step.name(),
                valid
            );
            let result = if valid {
                state.editing.store(true, Ordering::Release);
                struct Editing<'a>(&'a AtomicBool);
                impl Drop for Editing<'_> {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _editing = Editing(&state.editing);
                service.apply_edit(pending, ec)
            } else {
                Ok(())
            };
            if service.generation.load(Ordering::Acquire) != self.generation
                || service.edit_ticket.load(Ordering::Acquire) != self.ticket
            {
                return result;
            }
            service.edit_requested.store(false, Ordering::Release);
            if let Err(error) = &result {
                // A failed write may already have changed the document. Do not
                // replay the response or continue with dependent edit steps.
                service
                    .faulted
                    .mark("edit.apply_failed", error.code().0 as u32 as u64);
                let discarded = std::mem::take(&mut *service.lock(&service.pending_edit)?);
                drop(discarded);
                service.lock(&service.rpc)?.log(
                    "error",
                    format!("edit failed; service suspended: {error:?}"),
                );
            } else {
                // Avoid recursively requesting another write lock from inside
                // DoEditSession. The message window resumes the scheduler.
                let hwnd = service
                    .lock(&service.update_window)?
                    .as_ref()
                    .map(|w| w.hwnd);
                if let Some(hwnd) = hwnd {
                    if !unsafe {
                        bindings::PostMessageW(
                            Some(hwnd),
                            update_window::UPDATE_MESSAGE,
                            WPARAM(0),
                            LPARAM(0),
                        )
                    }
                    .as_bool()
                    {
                        let error = Error::from_thread();
                        service
                            .faulted
                            .mark("edit.post_failed", error.code().0 as u32 as u64);
                        return Err(error);
                    }
                }
            }
            result
        });
        weasel_common::input_trace!(
            "edit.exit ticket={} hr={:?} elapsed_us={}",
            self.ticket,
            result.as_ref().err().map(|e| e.code()),
            started.elapsed().as_micros()
        );
        result
    }
}
