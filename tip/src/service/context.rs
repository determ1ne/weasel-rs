//! Context-local state. Never route a reply through whichever input happens to
//! be focused when a pipe notification is dispatched.
use super::*;
use weasel_common::message::{ContextAction, ContextCommand, ContextToken};

static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

pub(super) struct ContextState {
    pub context: ITfContext,
    identity: IUnknown,
    pub id: u64,
    pub alive: AtomicBool,
    pub suspended: AtomicBool,
    pub generation: AtomicU64,
    pub rpc: Arc<Mutex<RpcWorker>>,
    pub composition: Mutex<Option<ITfComposition>>,
    pub composition_epoch: AtomicU64,
    pub disconnect_requested: AtomicBool,
    pub composition_text: Mutex<String>,
    pub composition_cursor: Mutex<usize>,
    pub layout: Mutex<layout::LayoutSchedule>,
    pub editing: AtomicBool,
    pub reconciling: AtomicBool,
    pub host_selection: Mutex<Option<(ITfRange, TF_SELECTIONSTYLE)>>,
    pub route: Mutex<ResponseRoute>,
    // Engine mode belongs to a connection epoch, not merely a document.
    pub input_mode: Mutex<Option<(u64, bool)>>,
    cookies: Mutex<Vec<u32>>,
}

#[derive(Default)]
pub(super) struct ResponseRoute {
    epoch: u64,
    revision: u64,
}

impl ResponseRoute {
    fn accept(&mut self, expected: &ContextToken, response: &KeyEventResponse) -> bool {
        if response.token.as_ref() != Some(expected) || expected.connection_epoch == 0 {
            return false;
        }
        if self.epoch != expected.connection_epoch {
            self.epoch = expected.connection_epoch;
            self.revision = 0;
        }
        if response.revision <= self.revision {
            return false;
        }
        self.revision = response.revision;
        true
    }
}

impl ContextState {
    pub fn token(&self) -> Result<ContextToken> {
        let rpc = self
            .rpc
            .try_lock()
            .map_err(|_| Error::from_hresult(boundary::E_FAIL))?;
        Ok(ContextToken {
            context_id: self.id,
            connection_epoch: rpc.connection_epoch(),
            generation: self.generation.load(Ordering::Acquire),
        })
    }

    pub fn matches(&self, token: Option<&ContextToken>) -> Result<bool> {
        Ok(self.alive.load(Ordering::Acquire)
            && !self.suspended.load(Ordering::Acquire)
            && token == Some(&self.token()?))
    }

    pub fn stop(&self) -> bool {
        self.alive.store(false, Ordering::Release);
        if self.editing.load(Ordering::Acquire) {
            return false;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        let resources = (|| {
            let mut cookies = boundary::try_teardown(&self.cookies)?;
            let mut composition = boundary::try_teardown(&self.composition)?;
            let mut selection = boundary::try_teardown(&self.host_selection)?;
            Some((
                std::mem::take(&mut *cookies),
                composition.take(),
                selection.take(),
            ))
        })();
        let Some((cookies, composition, selection)) = resources else {
            return false;
        };
        if let Ok(source) = self.context.cast::<ITfSource>() {
            for cookie in cookies {
                unsafe {
                    let _ = source.UnadviseSink(cookie);
                }
            }
        }
        drop(composition);
        drop(selection);
        true
    }
}

impl Drop for ContextState {
    fn drop(&mut self) {
        boundary::cleanup(|| {
            self.stop();
        });
    }
}

impl TextService {
    pub(super) fn find_context(&self, context: &ITfContext) -> Result<Option<Arc<ContextState>>> {
        let identity: IUnknown = context.cast()?;
        Ok(self
            .lock(&self.contexts)?
            .iter()
            .find(|state| state.identity == identity)
            .cloned())
    }

    pub(super) fn ensure_context(
        &self,
        context: ITfContext,
        owner: &IUnknown,
    ) -> Result<Arc<ContextState>> {
        if let Some(state) = self.find_context(&context)? {
            return Ok(state);
        }
        if !self.activated.load(Ordering::Acquire) || self.lock(&self.contexts)?.len() >= 32 {
            return Err(Error::from_hresult(boundary::E_FAIL));
        }
        let generation = self.generation.load(Ordering::Acquire);
        let state = Arc::new(ContextState {
            identity: context.cast()?,
            context,
            id: NEXT_CONTEXT.fetch_add(1, Ordering::AcqRel),
            alive: AtomicBool::new(true),
            suspended: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            rpc: self.rpc.clone(),
            composition: Mutex::new(None),
            composition_epoch: AtomicU64::new(0),
            disconnect_requested: AtomicBool::new(false),
            composition_text: Mutex::new(String::new()),
            composition_cursor: Mutex::new(0),
            layout: Mutex::default(),
            editing: AtomicBool::new(false),
            reconciling: AtomicBool::new(false),
            host_selection: Mutex::new(None),
            route: Mutex::new(ResponseRoute::default()),
            input_mode: Mutex::new(None),
            cookies: Mutex::new(Vec::new()),
        });
        let source: ITfSource = state.context.cast()?;
        let setup: Result<()> = (|| {
            for iid in [ITfTextEditSink::IID, ITfTextLayoutSink::IID] {
                let cookie = unsafe { source.AdviseSink(&iid, owner)? };
                self.lock(&state.cookies)?.push(cookie);
            }
            let hwnd = self
                .lock(&self.update_window)?
                .as_ref()
                .map(|w| w.hwnd.0 as usize)
                .unwrap_or(0);
            self.lock(&state.rpc)?.set_update_window(hwnd);
            if self.generation.load(Ordering::Acquire) != generation
                || !self.activated.load(Ordering::Acquire)
            {
                return Err(Error::from_hresult(boundary::E_FAIL));
            }
            Ok(())
        })();
        if let Err(error) = setup {
            state.stop();
            return Err(error);
        }
        self.lock(&self.contexts)?.push(state.clone());
        Ok(state)
    }

    pub(super) fn focus_context(&self, next: Option<Arc<ContextState>>) -> Result<()> {
        let id = next.as_ref().map(|state| state.id);
        let previous = {
            let mut focus = self.lock(&self.focused_context)?;
            if *focus == id {
                return Ok(());
            }
            std::mem::replace(&mut *focus, id)
        };
        self.lock(&self.tested_key)?.take();
        let states = self.lock(&self.contexts)?.clone();
        for state in states {
            if Some(state.id) == previous {
                self.send_context_action(&state, ContextAction::Blur, false)?;
            }
        }
        if let Some(state) = next {
            self.send_context_action(&state, ContextAction::Focus, false)?;
        }
        self.refresh_language_bar()?;
        Ok(())
    }

    pub(super) fn remove_context(&self, context: &ITfContext) -> Result<()> {
        let Some(state) = self.find_context(context)? else {
            return Ok(());
        };
        let was_focused = *self.lock(&self.focused_context)? == Some(state.id);
        if was_focused {
            self.focus_context(None)?;
        }
        if !state.stop() {
            self.faulted.request_maintenance();
            return Ok(());
        }
        let token = state.token()?;
        let _ = self.lock(&self.rpc)?.context_command(ContextCommand {
            token: Some(token),
            action: ContextAction::Destroy as i32,
        });
        let removed = {
            let mut states = self.lock(&self.contexts)?;
            states
                .iter()
                .position(|s| s.id == state.id)
                .map(|index| states.remove(index))
        };
        if self.active_edit_context.load(Ordering::Acquire) == state.id {
            self.edit_ticket.fetch_add(1, Ordering::AcqRel);
            self.edit_requested.store(false, Ordering::Release);
        }
        let discarded = {
            let mut queue = self.lock(&self.pending_edit)?;
            let old = std::mem::take(&mut *queue);
            let (keep, discard): (VecDeque<PendingEdit>, VecDeque<PendingEdit>) = old
                .into_iter()
                .partition(|task: &PendingEdit| task.state.id != state.id);
            *queue = keep;
            discard
        };
        drop(discarded);
        drop(removed);
        self.schedule_edit()?;
        Ok(())
    }

    pub(super) fn send_context_action(
        &self,
        state: &ContextState,
        action: ContextAction,
        invalidate: bool,
    ) -> Result<()> {
        if state.suspended.load(Ordering::Acquire) && action != ContextAction::Blur {
            return Ok(());
        }
        if invalidate {
            state.reconciling.store(true, Ordering::Release);
            state.generation.fetch_add(1, Ordering::AcqRel);
            self.lock(&self.tested_key)?.take();
            if self.active_edit_context.load(Ordering::Acquire) == state.id {
                self.edit_ticket.fetch_add(1, Ordering::AcqRel);
                self.edit_requested.store(false, Ordering::Release);
            }
        }
        let command = ContextCommand {
            token: Some(state.token()?),
            action: action as i32,
        };
        let result = self.lock(&state.rpc)?.context_command(command);
        if let Err(error) = result {
            self.faulted.event(error.name(), action as u64);
            // The worker invalidates its epoch on rejection. A failed context
            // transition is not replayable while a composition may exist.
            if self.lock(&state.composition)?.is_some() || invalidate {
                self.quarantine(state, "context.transition_failed", action as u64);
            }
            return Ok(());
        }
        Ok(())
    }

    pub(super) fn drain_context_updates(&self, owner: &IUnknown) -> Result<()> {
        let states = self.lock(&self.contexts)?.clone();
        // Remove dead destinations, but leave other contexts queued if one
        // context's fallible TSF processing exits early during reentrancy.
        let destinations: Vec<_> = states
            .iter()
            .filter(|state| {
                state.alive.load(Ordering::Acquire) && !state.suspended.load(Ordering::Acquire)
            })
            .map(|state| state.id)
            .collect();
        self.lock(&self.rpc)?.retain_context_updates(&destinations);
        for state in states {
            if state.suspended.load(Ordering::Acquire) || !state.alive.load(Ordering::Acquire) {
                continue;
            }
            if self.cleanup_disconnected_composition(&state, owner)? {
                continue;
            }
            let responses = self.lock(&self.rpc)?.take_context_updates(state.id);
            for response in responses {
                let token = state.token()?;
                if !state.alive.load(Ordering::Acquire)
                    || !self.lock(&state.route)?.accept(&token, &response)
                {
                    weasel_common::input_trace!(
                        "reply.drop expected={:?} token={:?} revision={}",
                        token,
                        response.token,
                        response.revision
                    );
                    continue;
                }
                weasel_common::input_trace!(
                    "reply.accept token={:?} revision={} state={} preedit_bytes={} commit_bytes={}",
                    response.token,
                    response.revision,
                    response.state_updated,
                    response.composition.len(),
                    response.commit_text.len()
                );
                state.reconciling.store(false, Ordering::Release);
                if let Some(ascii) = response.ascii_mode {
                    *self.lock(&state.input_mode)? = Some((token.connection_epoch, ascii));
                    self.refresh_language_bar()?;
                }
                if response::has_edit_payload(&response) {
                    let needs_edit = !response.commit_text.is_empty()
                        || response.composing != self.lock(&state.composition)?.is_some()
                        || response.composition != *self.lock(&state.composition_text)?
                        || response.composition_cursor as usize
                            != *self.lock(&state.composition_cursor)?
                        || response.open_emoji_panel
                        || self
                            .lock(&self.pending_edit)?
                            .iter()
                            .any(|task| task.state.id == state.id)
                        || (self.edit_requested.load(Ordering::Acquire)
                            && self.active_edit_context.load(Ordering::Acquire) == state.id);
                    if needs_edit {
                        self.request_edit_session(
                            state.context.clone(),
                            response,
                            EditStep::ApplyResponse,
                            owner.clone(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl TextService {
    pub(super) fn cleanup_disconnected_composition(
        &self,
        state: &Arc<ContextState>,
        owner: &IUnknown,
    ) -> Result<bool> {
        let epoch = state.token()?.connection_epoch;
        if self.lock(&state.composition)?.is_none() {
            return Ok(false);
        }
        if state.composition_epoch.load(Ordering::Acquire) == epoch {
            return Ok(false);
        }
        if state.disconnect_requested.swap(true, Ordering::AcqRel) {
            return Ok(true);
        }
        state.generation.fetch_add(1, Ordering::AcqRel);
        let result = (|| {
            self.lock(&self.tested_key)?.take();
            self.discard_context_edits(state.id)?;
            let response = KeyEventResponse {
                token: Some(state.token()?),
                ..Default::default()
            };
            self.request_edit_session(
                state.context.clone(),
                response,
                EditStep::DisconnectComposition,
                owner.clone(),
            )
        })();
        if let Err(error) = result {
            state.disconnect_requested.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(self.lock(&state.composition)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_context_old_connection_generation_and_revision() {
        let token = ContextToken {
            context_id: 3,
            connection_epoch: 7,
            generation: 2,
        };
        let mut route = ResponseRoute::default();
        let mut response = KeyEventResponse {
            token: Some(token.clone()),
            revision: 1,
            ..Default::default()
        };
        assert!(route.accept(&token, &response));
        assert!(!route.accept(&token, &response));
        for bad in [
            ContextToken {
                context_id: 4,
                ..token.clone()
            },
            ContextToken {
                connection_epoch: 6,
                ..token.clone()
            },
            ContextToken {
                generation: 1,
                ..token.clone()
            },
        ] {
            response.token = Some(bad);
            response.revision = 99;
            assert!(!route.accept(&token, &response));
        }
        let fresh = ContextToken {
            connection_epoch: 8,
            ..token
        };
        response.token = Some(fresh.clone());
        response.revision = 1;
        assert!(route.accept(&fresh, &response));
    }
}
