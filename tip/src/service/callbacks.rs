use super::*;

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<'_, ITfThreadMgr>, tid: TfClientId) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.activate_tsf(ptim, tid, self.to_interface())
        })
    }

    fn Deactivate(&self) -> Result<()> {
        boundary::guard(None, || {
            self.deactivate_internal();
            Ok(())
        })
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(
        &self,
        ptim: Ref<'_, ITfThreadMgr>,
        tid: TfClientId,
        _dwflags: u32,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.activate_tsf(ptim, tid, self.to_interface())
        })
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<'_, ITfDocumentMgr>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || Ok(()))
    }

    fn OnUninitDocumentMgr(&self, pdim: Ref<'_, ITfDocumentMgr>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            if let Some(document) = pdim.to_owned() {
                let states = self.lock(&self.contexts)?.clone();
                for state in states {
                    if unsafe { state.context.GetDocumentMgr().ok() }.as_ref() == Some(&document) {
                        self.remove_context(&state.context)?;
                    }
                }
            }
            Ok(())
        })
    }

    fn OnSetFocus(
        &self,
        pdimfocus: Ref<'_, ITfDocumentMgr>,
        _pdimprevfocus: Ref<'_, ITfDocumentMgr>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.set_text_edit_sink(
                pdimfocus.to_owned(),
                self.to_interface(),
                self.to_interface(),
            )?;
            Ok(())
        })
    }

    fn OnPushContext(&self, pic: Ref<'_, ITfContext>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            if let Some(context) = pic.to_owned() {
                self.ensure_context(context, &self.to_interface())?;
            }
            self.subscribe_to_focused_context(self.to_interface(), self.to_interface())
        })
    }

    fn OnPopContext(&self, pic: Ref<'_, ITfContext>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            if let Some(context) = pic.to_owned() {
                self.remove_context(&context)?;
            }
            self.subscribe_to_focused_context(self.to_interface(), self.to_interface())
        })
    }
}

impl ITfTextEditSink_Impl for TextService_Impl {
    fn OnEndEdit(
        &self,
        pic: Ref<'_, ITfContext>,
        ecreadonly: TfEditCookie,
        peditrecord: Ref<'_, ITfEditRecord>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.on_host_edit(pic, ecreadonly, peditrecord)
        })
    }
}

impl ITfTextLayoutSink_Impl for TextService_Impl {
    fn OnLayoutChange(
        &self,
        pic: Ref<'_, ITfContext>,
        _lcode: TfLayoutCode,
        pview: Ref<'_, ITfContextView>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            let Some(context) = pic.to_owned() else {
                return Ok(());
            };
            let Some(view) = pview.to_owned() else {
                return Ok(());
            };
            let Some(state) = self.find_context(&context)? else {
                return Ok(());
            };
            if *self.lock(&self.focused_context)? != Some(state.id) {
                return Ok(());
            }
            let Some(composition) = self.lock(&state.composition)?.as_ref().cloned() else {
                let token = state.token()?;
                self.lock(&state.rpc)?.send_layout_update(LayoutUpdate {
                    session_id: state.id,
                    token: Some(token),
                    anchor: Some(RenderRect::default()),
                });
                return Ok(());
            };
            let Ok(range) = (unsafe { composition.GetRange() }) else {
                return Ok(());
            };
            let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
                return Ok(());
            };
            let probe: ITfEditSession = LayoutProbe {
                view,
                range,
                state: state.clone(),
                token: state.token()?,
                generation: Arc::clone(&self.generation),
                requested_generation: self.generation.load(Ordering::Acquire),
                _module: ModuleLease::new(),
            }
            .into();
            let _ = unsafe { context.RequestEditSession(tid, &probe, TF_ES_READ) };
            Ok(())
        })
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.lock(&self.tested_key)?.take();
            Ok(())
        })
    }

    fn OnTestKeyDown(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        let started = std::time::Instant::now();
        weasel_common::input_trace!(
            "callback.begin name=OnTestKeyDown vk={} lp={}",
            wparam.0,
            lparam.0
        );
        let result = boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, false, true, self.to_interface())
        });
        weasel_common::input_trace!(
            "callback.end name=OnTestKeyDown vk={} lp={} eaten={:?} hr={:?} elapsed_us={}",
            wparam.0,
            lparam.0,
            result.as_ref().ok().map(|v| v.as_bool()),
            result.as_ref().err().map(|e| e.code()),
            started.elapsed().as_micros()
        );
        result
    }

    fn OnTestKeyUp(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        let started = std::time::Instant::now();
        weasel_common::input_trace!(
            "callback.begin name=OnTestKeyUp vk={} lp={}",
            wparam.0,
            lparam.0
        );
        let result = boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, true, true, self.to_interface())
        });
        weasel_common::input_trace!(
            "callback.end name=OnTestKeyUp vk={} lp={} eaten={:?} hr={:?} elapsed_us={}",
            wparam.0,
            lparam.0,
            result.as_ref().ok().map(|v| v.as_bool()),
            result.as_ref().err().map(|e| e.code()),
            started.elapsed().as_micros()
        );
        result
    }

    fn OnKeyDown(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        let started = std::time::Instant::now();
        weasel_common::input_trace!(
            "callback.begin name=OnKeyDown vk={} lp={}",
            wparam.0,
            lparam.0
        );
        let result = boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, false, false, self.to_interface())
        });
        weasel_common::input_trace!(
            "callback.end name=OnKeyDown vk={} lp={} eaten={:?} hr={:?} elapsed_us={}",
            wparam.0,
            lparam.0,
            result.as_ref().ok().map(|v| v.as_bool()),
            result.as_ref().err().map(|e| e.code()),
            started.elapsed().as_micros()
        );
        result
    }

    fn OnKeyUp(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        let started = std::time::Instant::now();
        weasel_common::input_trace!(
            "callback.begin name=OnKeyUp vk={} lp={}",
            wparam.0,
            lparam.0
        );
        let result = boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, true, false, self.to_interface())
        });
        weasel_common::input_trace!(
            "callback.end name=OnKeyUp vk={} lp={} eaten={:?} hr={:?} elapsed_us={}",
            wparam.0,
            lparam.0,
            result.as_ref().ok().map(|v| v.as_bool()),
            result.as_ref().err().map(|e| e.code()),
            started.elapsed().as_micros()
        );
        result
    }

    fn OnPreservedKey(&self, _pic: Ref<'_, ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        boundary::guard(Some(&self.faulted), || Ok(BOOL(0)))
    }
}

impl ITfThreadFocusSink_Impl for TextService_Impl {
    fn OnSetThreadFocus(&self) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.subscribe_to_focused_context(self.to_interface(), self.to_interface())
        })
    }

    fn OnKillThreadFocus(&self) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.lock(&self.tested_key)?.take();
            self.focus_context(None)?;
            Ok(())
        })
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: TfEditCookie,
        pcomposition: Ref<'_, ITfComposition>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            let Some(terminated) = pcomposition.to_owned() else {
                return Ok(());
            };
            let states = self.lock(&self.contexts)?.clone();
            for state in states {
                let removed = {
                    let mut current = self.lock(&state.composition)?;
                    if current.as_ref() != Some(&terminated) {
                        continue;
                    }
                    current.take()
                };
                drop(removed);
                let saved = self.lock(&state.host_selection)?.take();
                drop(saved);
                self.lock(&state.composition_text)?.clear();
                *self.lock(&state.composition_cursor)? = 0;
                // A locally initiated EndComposition takes the reference first,
                // so only host-initiated termination reaches this branch.
                self.send_context_action(
                    &state,
                    weasel_common::message::ContextAction::HostTerminated,
                    true,
                )?;
                break;
            }
            Ok(())
        })
    }
}

impl ITfActiveLanguageProfileNotifySink_Impl for TextService_Impl {
    fn OnActivated(
        &self,
        _clsid: *const GUID,
        _guidprofile: *const GUID,
        _factivated: BOOL,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || Ok(()))
    }
}

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        boundary::guard(Some(&self.faulted), || {
            Ok(DisplayAttributeEnumerator::new().into())
        })
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        boundary::guard(Some(&self.faulted), || {
            if guid.is_null() || unsafe { *guid } != GUID_WEASEL_DISPLAY_ATTRIBUTE {
                return Err(Error::from_hresult(E_NOINTERFACE));
            }
            Ok(DisplayAttributeInfo::new().into())
        })
    }
}

impl ITfCompartmentEventSink_Impl for TextService_Impl {
    fn OnChange(&self, _rguid: *const GUID) -> Result<()> {
        boundary::guard(Some(&self.faulted), || Ok(()))
    }
}
