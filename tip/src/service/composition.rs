use super::*;
use crate::bindings::TF_E_READONLY;

impl TextService {
    pub(super) fn request_edit_session(
        &self,
        context: ITfContext,
        response: KeyEventResponse,
        step: EditStep,
        session: IUnknown,
    ) -> Result<()> {
        self.faulted.event(step.name(), response.revision);
        if let Some(token) = &response.token {
            self.faulted.event("edit.context", token.context_id);
            self.faulted.event("edit.epoch", token.connection_epoch);
            self.faulted.event("edit.generation", token.generation);
        }
        if response::validate(&response).is_err() {
            if let Some(state) = self.find_context(&context)? {
                self.quarantine(&state, "response.invalid", response.revision);
            }
            return Ok(());
        }
        let Some(state) = self.find_context(&context)? else {
            return Ok(());
        };
        if !matches!(step, EditStep::DisconnectComposition)
            && !state.matches(response.token.as_ref())?
        {
            return Ok(());
        }
        let pending = PendingEdit {
            state,
            context,
            response,
            step,
            session,
        };
        weasel_common::input_trace!(
            "edit.queue token={:?} revision={} step={}",
            pending.response.token,
            pending.response.revision,
            step.name()
        );
        {
            let mut queue = self.lock(&self.pending_edit)?;
            if queue.len() >= 64 {
                self.quarantine(&pending.state, "edit.queue_full", queue.len() as u64);
                return Ok(());
            }
            if matches!(step, EditStep::ApplyResponse) {
                queue.push_back(pending);
            } else {
                queue.push_front(pending);
            }
        }
        self.schedule_edit()?;
        Ok(())
    }

    pub(super) fn schedule_edit(&self) -> Result<()> {
        if !self.activated.load(Ordering::Acquire) || self.faulted.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.edit_requested.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let generation = self.generation.load(Ordering::Acquire);
        let ticket = self.edit_ticket.fetch_add(1, Ordering::AcqRel) + 1;
        let mut reservation = safety::EditReservation::new(self, ticket, generation);
        let pending = loop {
            let next = self.lock(&self.pending_edit)?.pop_front();
            let Some(pending) = next else {
                self.edit_requested.store(false, Ordering::Release);
                return Ok(());
            };
            let matches = match pending.matches() {
                Ok(matches) => matches,
                Err(error) => {
                    self.quarantine(
                        &pending.state,
                        "edit.prepare_failed",
                        error.code().0 as u32 as u64,
                    );
                    return Err(error);
                }
            };
            if matches {
                break pending;
            }
        };
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            let discarded = std::mem::take(&mut *self.lock(&self.pending_edit)?);
            drop(discarded);
            self.edit_requested.store(false, Ordering::Release);
            self.lock(&self.rpc)?
                .log("warn", "key response could not be edited: no TSF client id");
            return Ok(());
        };

        let context = pending.context.clone();
        let step = pending.step;
        let state = pending.state.clone();
        self.active_edit_context
            .store(pending.state.id, Ordering::Release);
        self.faulted.event("edit.ticket", ticket);
        weasel_common::input_trace!(
            "edit.request ticket={} token={:?} revision={} step={}",
            ticket,
            pending.response.token,
            pending.response.revision,
            step.name()
        );
        let session: ITfEditSession =
            edit_session::ResponseEdit::new(self, pending, generation, ticket).into();
        let request = unsafe {
            context.RequestEditSession(tid, &session, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)
        };
        weasel_common::input_trace!(
            "edit.request_result ticket={} step={} result={:?}",
            ticket,
            step.name(),
            request
        );
        if self.edit_ticket.load(Ordering::Acquire) != ticket
            || self.generation.load(Ordering::Acquire) != generation
        {
            return Ok(());
        }
        match request {
            Ok(hr) if hr.0 == TF_E_READONLY => {
                self.reject_readonly_edit(&state)?;
            }
            Err(ref error) if error.code().0 == TF_E_READONLY => {
                self.reject_readonly_edit(&state)?;
            }
            Err(error) => {
                self.quarantine(&state, "edit.request_failed", error.code().0 as u32 as u64);
                self.discard_context_edits(state.id)?;
                self.edit_requested.store(false, Ordering::Release);
                self.lock(&self.rpc)?.log(
                    "error",
                    format!(
                        "RequestEditSession call failed step={} hr={error:?}",
                        step.name()
                    ),
                );
            }
            Ok(hr) if hr.is_err() => {
                self.quarantine(&state, "edit.session_failed", hr.0 as u32 as u64);
                self.discard_context_edits(state.id)?;
                self.edit_requested.store(false, Ordering::Release);
                self.lock(&self.rpc)?.log(
                    "error",
                    format!(
                        "edit session returned failure step={} hr={hr:?}",
                        step.name()
                    ),
                );
            }
            Ok(_) => reservation.hand_off(),
        }
        Ok(())
    }

    pub(super) fn start_composition(
        &self,
        context: &ITfContext,
        response: &KeyEventResponse,
        ec: TfEditCookie,
        sink: &ITfCompositionSink,
        session: &IUnknown,
    ) -> Result<()> {
        let state = self
            .find_context(context)?
            .ok_or_else(|| Error::from_hresult(boundary::E_FAIL))?;
        if !state.matches(response.token.as_ref())? {
            return Ok(());
        }

        let insert: ITfInsertAtSelection = context.cast()?;
        let range = unsafe { insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, ptr::null(), 0)? };
        let context_composition: ITfContextComposition = context.cast()?;
        self.edit_mutated.store(true, Ordering::Release);
        let composition = unsafe { context_composition.StartComposition(ec, &range, sink)? };
        let previous = self.lock(&state.composition)?.replace(composition);
        state.composition_epoch.store(
            response.token.as_ref().map_or(0, |t| t.connection_epoch),
            Ordering::Release,
        );
        self.lock(&state.rpc)?.reset_layout();
        drop(previous);
        self.lock(&state.composition_text)?.clear();
        *self.lock(&state.composition_cursor)? = 0;
        self.set_selection(context, ec, &range, 0)?;

        let next = if !response.commit_text.is_empty() {
            EditStep::CommitComposition
        } else {
            EditStep::UpdateComposition
        };
        self.request_edit_session(context.clone(), response.clone(), next, session.clone())?;
        Ok(())
    }

    pub(super) fn update_composition(
        &self,
        context: &ITfContext,
        response: &KeyEventResponse,
        ec: TfEditCookie,
        _session: &IUnknown,
    ) -> Result<()> {
        let state = self
            .find_context(context)?
            .ok_or_else(|| Error::from_hresult(boundary::E_FAIL))?;
        if !state.matches(response.token.as_ref())? {
            return Ok(());
        }

        let active = self
            .lock(&state.composition)?
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::from_hresult(E_POINTER))?;
        let range = unsafe { active.GetRange()? };
        self.set_range_text(&range, ec, &response.composition)?;
        // Display attributes are optional.  Continue the edit session when
        // the host does not expose a writable attribute property.
        self.set_display_attribute_best_effort(context, ec, &range);
        self.set_selection(context, ec, &range, response.composition_cursor as usize)?;
        *self.lock(&state.composition_text)? = response.composition.clone();
        *self.lock(&state.composition_cursor)? = response.composition_cursor as usize;
        // Geometry failure must not fault or replay a successful text edit.
        // Layout notifications can still supply the position later.
        let _ = self.request_composition_layout(&state);
        Ok(())
    }

    pub(super) fn insert_commit(
        &self,
        context: &ITfContext,
        response: &KeyEventResponse,
        ec: TfEditCookie,
        session: &IUnknown,
    ) -> Result<()> {
        let state = self
            .find_context(context)?
            .ok_or_else(|| Error::from_hresult(boundary::E_FAIL))?;
        if !state.matches(response.token.as_ref())? {
            return Ok(());
        }

        let insert: ITfInsertAtSelection = context.cast()?;
        let utf16: Vec<u16> = response.commit_text.encode_utf16().collect();
        self.edit_mutated.store(true, Ordering::Release);
        let range = unsafe {
            insert.InsertTextAtSelection(
                ec,
                0,
                utf16.as_ptr(),
                utf16.len().try_into().unwrap_or(i32::MAX),
            )?
        };
        self.collapse_end(&range, ec)?;
        self.set_selection(context, ec, &range, 0)?;
        if response.composing {
            let mut response = response.clone();
            response.commit_text.clear();
            self.request_edit_session(
                context.clone(),
                response,
                EditStep::StartComposition,
                session.clone(),
            )?;
        }
        Ok(())
    }

    pub(super) fn commit_composition(
        &self,
        context: &ITfContext,
        response: &KeyEventResponse,
        ec: TfEditCookie,
        session: &IUnknown,
    ) -> Result<()> {
        let state = self
            .find_context(context)?
            .ok_or_else(|| Error::from_hresult(boundary::E_FAIL))?;
        if !state.matches(response.token.as_ref())? {
            return Ok(());
        }

        let active = self
            .lock(&state.composition)?
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::from_hresult(E_POINTER))?;
        let range = unsafe { active.GetRange()? };
        self.clear_display_attribute_best_effort(context, ec, &range);
        self.set_range_text(&range, ec, &response.commit_text)?;
        self.collapse_end(&range, ec)?;
        self.set_selection(context, ec, &range, 0)?;
        self.request_edit_session(
            context.clone(),
            response.clone(),
            EditStep::EndComposition {
                clear: false,
                restart: response.composing,
            },
            session.clone(),
        )?;
        Ok(())
    }

    pub(super) fn end_composition(
        &self,
        context: &ITfContext,
        response: &KeyEventResponse,
        ec: TfEditCookie,
        clear: bool,
        restart: bool,
        session: &IUnknown,
    ) -> Result<()> {
        let state = self
            .find_context(context)?
            .ok_or_else(|| Error::from_hresult(boundary::E_FAIL))?;
        if !state.matches(response.token.as_ref())? {
            return Ok(());
        }

        let active = self.lock(&state.composition)?.take();
        if let Some(active) = active {
            let range = unsafe { active.GetRange()? };
            self.clear_display_attribute_best_effort(context, ec, &range);
            if clear {
                self.set_range_text(&range, ec, "")?;
            }
            self.lock(&state.composition_text)?.clear();
            *self.lock(&state.composition_cursor)? = 0;
            self.edit_mutated.store(true, Ordering::Release);
            let hr = unsafe { active.EndComposition(ec) };
            if hr.is_err() {
                return Err(Error::from_hresult(hr));
            }
            let saved = self.lock(&state.host_selection)?.take();
            if let Some((range, style)) = saved {
                self.restore_selection(context, ec, range, style)?;
            }
        }
        if restart {
            let mut response = response.clone();
            response.commit_text.clear();
            self.request_edit_session(
                context.clone(),
                response,
                EditStep::StartComposition,
                session.clone(),
            )?;
        }
        Ok(())
    }

    pub(super) fn apply_edit(&self, pending: PendingEdit, ec: TfEditCookie) -> Result<()> {
        let PendingEdit {
            state,
            context,
            response,
            step,
            session,
        } = pending;
        match step {
            EditStep::ApplyResponse => {
                if !response::has_edit_payload(&response) {
                    return Ok(());
                }
                let has_composition = self.lock(&state.composition)?.is_some();
                let step = if !response.commit_text.is_empty() {
                    if has_composition {
                        EditStep::CommitComposition
                    } else {
                        EditStep::InsertCommit
                    }
                } else if !response.composing {
                    EditStep::EndComposition {
                        clear: true,
                        restart: false,
                    }
                } else if has_composition {
                    EditStep::UpdateComposition
                } else {
                    EditStep::StartComposition
                };
                self.apply_edit(
                    PendingEdit {
                        state,
                        context,
                        response,
                        step,
                        session,
                    },
                    ec,
                )
            }
            EditStep::StartComposition => {
                let sink: ITfCompositionSink = session.cast()?;
                self.start_composition(&context, &response, ec, &sink, &session)
            }
            EditStep::UpdateComposition => {
                self.update_composition(&context, &response, ec, &session)
            }
            EditStep::CommitComposition => {
                self.commit_composition(&context, &response, ec, &session)
            }
            EditStep::InsertCommit => self.insert_commit(&context, &response, ec, &session),
            EditStep::DisconnectComposition => {
                struct Reset<'a>(&'a AtomicBool);
                impl Drop for Reset<'_> {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _reset = Reset(&state.disconnect_requested);
                // Preserve preedit as plain text, without committing a candidate,
                // deleting host text, or restoring an obsolete selection.
                let active = self.lock(&state.composition)?.clone();
                if let Some(active) = active {
                    let range = unsafe { active.GetRange()? };
                    let owned = self.lock(&state.composition)?.take();
                    drop(owned);
                    self.clear_display_attribute_best_effort(&context, ec, &range);
                    self.edit_mutated.store(true, Ordering::Release);
                    unsafe {
                        active.EndComposition(ec).ok()?;
                    }
                }
                self.lock(&state.composition_text)?.clear();
                *self.lock(&state.composition_cursor)? = 0;
                let saved = self.lock(&state.host_selection)?.take();
                drop(saved);
                state.composition_epoch.store(0, Ordering::Release);
                Ok(())
            }
            EditStep::EndComposition { clear, restart } => {
                self.end_composition(&context, &response, ec, clear, restart, &session)?;
                if response.open_emoji_panel {
                    if let Some(window) = self.lock(&self.update_window)?.as_ref() {
                        window.open_emoji_after_edit();
                    }
                }
                Ok(())
            }
        }
    }
}
