use super::*;

impl TextService {
    pub(super) fn request_edit_session(
        &self,
        context: ITfContext,
        response: KeyEventResponse,
        step: EditStep,
        session: IUnknown,
    ) -> Result<()> {
        if response::validate(&response).is_err() {
            self.faulted.store(true, Ordering::Release);
            return Ok(());
        }
        let Some(state) = self.find_context(&context)? else {
            return Ok(());
        };
        if !state.matches(response.token.as_ref())? {
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
                self.faulted.store(true, Ordering::Release);
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
        let pending = loop {
            let next = self.lock(&self.pending_edit)?.pop_front();
            let Some(pending) = next else {
                self.edit_requested.store(false, Ordering::Release);
                return Ok(());
            };
            if pending.state.matches(pending.response.token.as_ref())? {
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
        let generation = self.generation.load(Ordering::Acquire);
        self.active_edit_context
            .store(pending.state.id, Ordering::Release);
        let ticket = self.edit_ticket.fetch_add(1, Ordering::AcqRel) + 1;
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
            Err(error) => {
                self.faulted.store(true, Ordering::Release);
                let discarded = std::mem::take(&mut *self.lock(&self.pending_edit)?);
                drop(discarded);
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
                self.faulted.store(true, Ordering::Release);
                let discarded = std::mem::take(&mut *self.lock(&self.pending_edit)?);
                drop(discarded);
                self.edit_requested.store(false, Ordering::Release);
                self.lock(&self.rpc)?.log(
                    "error",
                    format!(
                        "edit session returned failure step={} hr={hr:?}",
                        step.name()
                    ),
                );
            }
            Ok(_) => {}
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
        let composition = unsafe { context_composition.StartComposition(ec, &range, sink)? };
        let previous = self.lock(&state.composition)?.replace(composition);
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
