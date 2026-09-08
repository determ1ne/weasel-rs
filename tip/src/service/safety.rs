//! Scheduler ownership and context-local quarantine. Never replay uncertain writes.
use super::*;

pub(super) struct EditReservation<'a> {
    service: &'a TextService,
    ticket: u64,
    generation: u64,
    handed_off: bool,
}
impl<'a> EditReservation<'a> {
    pub fn new(service: &'a TextService, ticket: u64, generation: u64) -> Self {
        Self {
            service,
            ticket,
            generation,
            handed_off: false,
        }
    }
    pub fn hand_off(&mut self) {
        self.handed_off = true;
    }
}
impl Drop for EditReservation<'_> {
    fn drop(&mut self) {
        if !self.handed_off
            && self.service.edit_ticket.load(Ordering::Acquire) == self.ticket
            && self.service.generation.load(Ordering::Acquire) == self.generation
        {
            self.service.edit_requested.store(false, Ordering::Release);
        }
    }
}

impl TextService {
    pub(super) fn reject_readonly_edit(&self, state: &ContextState) -> Result<()> {
        self.faulted.event("edit.readonly", state.id);
        self.lock(&self.tested_key)?.take();
        self.discard_context_edits(state.id)?;
        state.generation.fetch_add(1, Ordering::AcqRel);
        let command = weasel_common::message::ContextCommand {
            token: Some(state.token()?),
            action: weasel_common::message::ContextAction::Cancel as i32,
            ascii_mode: None,
        };
        // Invalidate the cancellation reply too: it must not request another
        // write session against this read-only document.
        state.generation.fetch_add(1, Ordering::AcqRel);
        state.composition_epoch.store(0, Ordering::Release);
        state.disconnect_requested.store(false, Ordering::Release);
        self.edit_requested.store(false, Ordering::Release);
        let result = self.lock(&state.rpc)?.context_command(command);
        if let Err(error) = result {
            self.faulted.event(error.name(), state.id);
        }
        // Existing host text is left untouched. Once writable, the normal
        // disconnected-composition path ends any remaining composition.
        Ok(())
    }

    pub(super) fn quarantine(&self, state: &ContextState, reason: &'static str, code: u64) {
        if state.suspended.swap(true, Ordering::AcqRel) {
            return;
        }
        state.generation.fetch_add(1, Ordering::AcqRel);
        self.faulted.report_quarantine(state.id, reason, code);
        self.faulted.request_maintenance();
        // Generation/suspended reject this context's replies. Do not invalidate
        // the shared transport and unrelated contexts for a local edit failure.
    }

    pub(super) fn discard_context_edits(&self, context_id: u64) -> Result<()> {
        let discarded = {
            let mut queue = self.lock(&self.pending_edit)?;
            let old = std::mem::take(&mut *queue);
            let (keep, discard) = old
                .into_iter()
                .partition::<VecDeque<_>, _>(|task| task.state.id != context_id);
            *queue = keep;
            discard
        };
        drop(discarded);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scheduler_prepare_error_releases_busy_flag() {
        let service = TextService::new();
        service.activated.store(true, Ordering::Release);
        let queue = service.pending_edit.lock().unwrap();
        assert_eq!(
            service.schedule_edit().unwrap_err().code(),
            boundary::E_PENDING
        );
        assert!(!service.edit_requested.load(Ordering::Acquire));
        assert!(!service.faulted.load(Ordering::Acquire));
        drop(queue);
        assert!(service.schedule_edit().is_ok());
    }

    #[test]
    fn successful_handoff_and_new_activation_are_not_cleared() {
        let service = TextService::new();
        service.edit_requested.store(true, Ordering::Release);
        let mut reservation = EditReservation::new(&service, 0, 0);
        reservation.hand_off();
        drop(reservation);
        assert!(service.edit_requested.load(Ordering::Acquire));
        let reservation = EditReservation::new(&service, 0, 0);
        service.generation.store(1, Ordering::Release);
        drop(reservation);
        assert!(service.edit_requested.load(Ordering::Acquire));
    }
    #[test]
    fn reservation_cleans_early_return_but_not_newer_request() {
        let service = TextService::new();
        service.edit_requested.store(true, Ordering::Release);
        {
            let _reservation = EditReservation::new(&service, 0, 0);
        }
        assert!(!service.edit_requested.load(Ordering::Acquire));
        service.edit_requested.store(true, Ordering::Release);
        let reservation = EditReservation::new(&service, 0, 0);
        service.edit_ticket.store(1, Ordering::Release);
        drop(reservation);
        assert!(service.edit_requested.load(Ordering::Acquire));
    }
}
