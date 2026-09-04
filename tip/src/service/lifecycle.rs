use super::*;

impl TextService {
    pub(super) fn activate_tsf(
        &self,
        thread_mgr: Ref<'_, ITfThreadMgr>,
        tid: TfClientId,
        owner: IUnknown,
    ) -> Result<()> {
        // The marker belongs to the TIP DLL directory, not the host EXE.
        weasel_common::input_diagnostics::enable(
            crate::registration::module_path()
                .ok()
                .and_then(|path| {
                    std::path::PathBuf::from(path.to_string_lossy())
                        .parent()
                        .map(|directory| directory.join(".dev").is_file())
                })
                .unwrap_or(false),
        );
        let result = (|| {
            self.activate_with_thread_manager(thread_mgr, tid, owner.cast()?, owner.cast()?)?;
            // Notifications must have a destination before creating context RPCs.
            self.start_update_window(owner.clone())?;
            let manager = self.lock(&self.thread_mgr)?.clone();
            if let Some(manager) = manager {
                match language_bar::LanguageBar::attach(&manager) {
                    Ok(bar) => *self.lock(&self.language_bar)? = Some(Arc::new(bar)),
                    Err(error) => self
                        .lock(&self.rpc)?
                        .log("warn", format!("language bar registration failed: {error}")),
                }
            }
            self.subscribe_to_focused_context(owner.cast()?, owner.cast()?)
        })();
        if result.is_err() {
            self.deactivate_internal();
        }
        result
    }

    pub(super) fn activate_with_thread_manager(
        &self,
        thread_mgr: Ref<'_, ITfThreadMgr>,
        tid: TfClientId,
        sink: ITfThreadMgrEventSink,
        key_sink: ITfKeyEventSink,
    ) -> Result<()> {
        // TSF normally activates a TIP once per instance, but a reactivation
        // must not leave the previous sink registered on the thread manager.
        self.deactivate_internal();

        let Some(thread_mgr) = thread_mgr.to_owned() else {
            return Err(Error::from_hresult(E_POINTER));
        };
        let source: ITfSource = thread_mgr.cast()?;
        let cookie = unsafe { source.AdviseSink(&ITfThreadMgrEventSink::IID, &sink)? };
        let keystroke_mgr: ITfKeystrokeMgr = match thread_mgr.cast() {
            Ok(manager) => manager,
            Err(error) => {
                unsafe {
                    let _ = source.UnadviseSink(cookie);
                }
                return Err(error);
            }
        };
        let hr = unsafe { keystroke_mgr.AdviseKeyEventSink(tid, &key_sink, true) };
        if hr.is_err() {
            unsafe {
                let _ = source.UnadviseSink(cookie);
            }
            return Err(Error::from_hresult(hr));
        }

        let focus_cookie = match unsafe { source.AdviseSink(&ITfThreadFocusSink::IID, &sink) } {
            Ok(cookie) => cookie,
            Err(error) => {
                unsafe {
                    let _ = keystroke_mgr.UnadviseKeyEventSink(tid);
                    let _ = source.UnadviseSink(cookie);
                }
                return Err(error);
            }
        };
        *self.lock(&self.thread_focus_sink_cookie)? = Some(focus_cookie);
        *self.lock(&self.thread_mgr)? = Some(thread_mgr);
        *self.lock(&self.thread_mgr_event_sink_cookie)? = Some(cookie);
        *self.lock(&self.keystroke_mgr)? = Some(keystroke_mgr);
        *self.lock(&self.keystroke_client_id)? = Some(tid);
        self.lock(&self.rpc)?.start();
        self.register_display_attribute()?;
        self.activated.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn set_text_edit_sink(
        &self,
        document_mgr: Option<ITfDocumentMgr>,
        edit_sink: ITfTextEditSink,
        _layout_sink: ITfTextLayoutSink,
    ) -> Result<()> {
        let context = document_mgr.and_then(|manager| unsafe { manager.GetTop().ok() });
        let next = match context {
            Some(context) => Some(self.ensure_context(context, &edit_sink.cast()?)?),
            None => None,
        };
        self.focus_context(next)
    }

    pub(super) fn subscribe_to_focused_context(
        &self,
        edit_sink: ITfTextEditSink,
        layout_sink: ITfTextLayoutSink,
    ) -> Result<()> {
        let thread_mgr = self.lock(&self.thread_mgr)?.clone();
        let document_mgr = thread_mgr.and_then(|thread_mgr| unsafe { thread_mgr.GetFocus().ok() });
        self.set_text_edit_sink(document_mgr, edit_sink, layout_sink)?;
        Ok(())
    }

    pub(super) fn deactivate_internal(&self) {
        // Invalidate callbacks before any external call can reenter the service.
        self.activated.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        use boundary::{cleanup, teardown_lock};
        let bar = teardown_lock(&self.language_bar).take();
        cleanup(|| drop(bar));
        // Extract owned resources under short locks; run Drop/COM outside locks.
        let mut rpc = std::mem::take(&mut *teardown_lock(&self.rpc));
        cleanup(|| rpc.stop());
        let window = teardown_lock(&self.update_window).take();
        cleanup(|| drop(window));
        teardown_lock(&self.tested_key).take();
        let pending = std::mem::take(&mut *teardown_lock(&self.pending_edit));
        cleanup(|| drop(pending));
        self.edit_requested.store(false, Ordering::Release);
        teardown_lock(&self.display_attribute_atom).take();
        teardown_lock(&self.focused_context).take();
        let states = std::mem::take(&mut *teardown_lock(&self.contexts));
        for state in states {
            cleanup(|| state.stop());
        }
        let keys = teardown_lock(&self.keystroke_mgr).take();
        let tid = teardown_lock(&self.keystroke_client_id).take();
        cleanup(|| {
            if let (Some(keys), Some(tid)) = (keys, tid) {
                unsafe {
                    let _ = keys.UnadviseKeyEventSink(tid);
                }
            }
        });
        let cookie = teardown_lock(&self.thread_mgr_event_sink_cookie).take();
        let focus_cookie = teardown_lock(&self.thread_focus_sink_cookie).take();
        let manager = teardown_lock(&self.thread_mgr).take();
        cleanup(|| {
            if let (Some(manager), Some(cookie)) = (manager, cookie) {
                if let Ok(source) = manager.cast::<ITfSource>() {
                    unsafe {
                        let _ = source.UnadviseSink(cookie);
                        if let Some(focus_cookie) = focus_cookie {
                            let _ = source.UnadviseSink(focus_cookie);
                        }
                    }
                }
            }
        });
    }

    pub(super) fn start_update_window(&self, session: IUnknown) -> Result<()> {
        let service = self as *const TextService;
        // The captured COM reference keeps this allocation alive. The window
        // is created, dispatched and destroyed on the TSF apartment, and is
        // explicitly released by Deactivate before any service state is freed.
        let window = update_window::UpdateWindow::new(move || unsafe {
            let service = &*service;
            let _ = boundary::guard(Some(&service.faulted), || {
                if !service.activated.load(Ordering::Acquire) {
                    return Ok(());
                }
                service.drain_context_updates(&session)?;
                service.refresh_language_bar()?;
                service.schedule_edit()?;
                Ok(())
            });
        })?;
        self.lock(&self.rpc)?
            .set_update_window(window.hwnd.0 as usize);
        let states = self.lock(&self.contexts)?.clone();
        for state in states {
            self.lock(&state.rpc)?
                .set_update_window(window.hwnd.0 as usize);
        }
        *self.lock(&self.update_window)? = Some(window);
        Ok(())
    }
}
