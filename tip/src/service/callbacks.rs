//! 实现 TIP 对 TSF 线程管理器、文本上下文、键盘、合成和显示属性接口的回调。
//!
//! 回调参数来自宿主 COM 边界，所有可能失败的业务处理都经 `boundary::guard` 隔离，避免
//! Rust panic 或内部错误越过 ABI。回调只在对应 TSF 公寓中使用传入接口；键盘路径在故障态
//! 直接将按键交还宿主，文档/焦点变化则更新上下文订阅与其生命周期。
use super::*;

impl ITfTextInputProcessor_Impl for TextService_Impl {
    /// 接受 TSF 激活请求，并在 COM 边界内完成线程管理器订阅及初始状态同步。
    fn Activate(&self, ptim: Ref<'_, ITfThreadMgr>, tid: TfClientId) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.activate_tsf(ptim, tid, self.to_interface(), false)
        })
    }

    /// 停用 TIP；内部清理可因重入延后到公寓更新窗口重试。
    fn Deactivate(&self) -> Result<()> {
        boundary::guard(None, || {
            self.deactivate_internal();
            Ok(())
        })
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    /// 处理带安全模式标志的 TSF 激活请求。
    fn ActivateEx(&self, ptim: Ref<'_, ITfThreadMgr>, tid: TfClientId, dwflags: u32) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.activate_tsf(
                ptim,
                tid,
                self.to_interface(),
                dwflags & TF_TMF_SECUREMODE as u32 != 0,
            )
        })
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    /// 文档管理器创建时无需额外订阅，仍通过边界保护统一处理故障状态。
    fn OnInitDocumentMgr(&self, _pdim: Ref<'_, ITfDocumentMgr>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || Ok(()))
    }

    /// 文档管理器销毁时移除其所有文本上下文，释放对应订阅与会话状态。
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

    /// 焦点文档变化时选取其栈顶上下文，并更新焦点上下文关联。
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

    /// 新上下文压栈时建立会话状态、安排安全字段探测并重新解析当前焦点。
    fn OnPushContext(&self, pic: Ref<'_, ITfContext>) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            if let Some(context) = pic.to_owned() {
                let state = self.ensure_context(context, &self.to_interface())?;
                self.request_secure_field_probe(&state, self.to_interface(), false)?;
            }
            self.subscribe_to_focused_context(self.to_interface(), self.to_interface())
        })
    }

    /// 上下文出栈时移除其状态，再按 TSF 当前焦点恢复订阅。
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
    /// 在宿主编辑事务结束时读取编辑记录并同步相关上下文状态。
    fn OnEndEdit(
        &self,
        pic: Ref<'_, ITfContext>,
        ecreadonly: TfEditCookie,
        peditrecord: Ref<'_, ITfEditRecord>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.on_host_edit(pic, ecreadonly, peditrecord, self.to_interface())
        })
    }
}

impl ITfTextLayoutSink_Impl for TextService_Impl {
    /// 仅为当前焦点上下文请求合成布局；非焦点或已移除上下文的通知会被忽略。
    fn OnLayoutChange(
        &self,
        pic: Ref<'_, ITfContext>,
        _lcode: TfLayoutCode,
        _pview: Ref<'_, ITfContextView>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            let Some(context) = pic.to_owned() else {
                return Ok(());
            };
            let Some(state) = self.find_context(&context)? else {
                return Ok(());
            };
            if *self.lock(&self.focused_context)? != Some(state.id) {
                return Ok(());
            }
            // Completion and blur responses hide the candidates; idle layout is irrelevant.
            let _ = self.request_composition_layout(&state);
            Ok(())
        })
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// 键盘焦点切换使先前 `OnTestKey` 的暂存结果失效。
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.lock(&self.tested_key)?.take();
            Ok(())
        })
    }

    /// 预判按键是否由 TIP 处理；故障态不拦截按键，避免影响宿主输入。
    fn OnTestKeyDown(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, false, true, self.to_interface())
        })
    }

    /// 预判按键释放是否由 TIP 处理；与按下阶段共用暂存判定。
    fn OnTestKeyUp(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, true, true, self.to_interface())
        })
    }

    /// 处理已由测试阶段确认的按键按下，并将处理结果交还 TSF。
    fn OnKeyDown(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, false, false, self.to_interface())
        })
    }

    /// 处理已由测试阶段确认的按键释放；故障时按键留给宿主。
    fn OnKeyUp(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        if self.faulted.load(Ordering::Acquire) {
            return Ok(BOOL(0));
        }

        boundary::guard(Some(&self.faulted), || {
            self.forward_key_event(pic, wparam, lparam, true, false, self.to_interface())
        })
    }

    /// 当前没有注册保留键，因此不声明处理任何保留键通知。
    fn OnPreservedKey(&self, _pic: Ref<'_, ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        boundary::guard(Some(&self.faulted), || Ok(BOOL(0)))
    }
}

impl ITfThreadFocusSink_Impl for TextService_Impl {
    /// TSF 线程获得焦点后重新查询其焦点文档和顶层上下文。
    fn OnSetThreadFocus(&self) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.subscribe_to_focused_context(self.to_interface(), self.to_interface())
        })
    }

    /// TSF 线程失去焦点时清除按键预判，并解除当前上下文焦点状态。
    fn OnKillThreadFocus(&self) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            self.lock(&self.tested_key)?.take();
            self.focus_context(None)?;
            Ok(())
        })
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    /// 处理宿主终止的合成：结束本地原始合成并向对应上下文报告终止动作。
    ///
    /// 通过比较合成接口定位所属上下文；本地发起的结束会先清除该引用，因此不会重复走
    /// 宿主终止分支。编辑 cookie 由 TSF 提供，仅在本次回调的写事务范围内使用。
    fn OnCompositionTerminated(
        &self,
        ecwrite: TfEditCookie,
        pcomposition: Ref<'_, ITfComposition>,
    ) -> Result<()> {
        boundary::guard(Some(&self.faulted), || {
            let Some(terminated) = pcomposition.to_owned() else {
                return Ok(());
            };
            let states = self.lock(&self.contexts)?.clone();
            for state in states {
                if self.lock(&state.composition)?.as_ref() != Some(&terminated) {
                    continue;
                }
                let finished = self.finish_raw_composition(&state, ecwrite, true);
                // A locally initiated EndComposition takes the reference first,
                // so only host-initiated termination reaches this branch.
                self.send_context_action(
                    &state,
                    weasel_common::message::ContextAction::HostTerminated,
                    true,
                )?;
                finished?;
                break;
            }
            Ok(())
        })
    }
}

impl ITfActiveLanguageProfileNotifySink_Impl for TextService_Impl {
    /// 语言配置文件切换通知目前不改变服务状态。
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
    /// 枚举本 TIP 提供的显示属性描述对象。
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        boundary::guard(Some(&self.faulted), || {
            Ok(DisplayAttributeEnumerator::new().into())
        })
    }

    /// 只响应本 TIP 的显示属性 GUID，其他 GUID 按 COM 约定返回无接口错误。
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
    /// compartment 变更通知目前不需额外处理，仍由边界守卫隔离故障。
    fn OnChange(&self, _rguid: *const GUID) -> Result<()> {
        boundary::guard(Some(&self.faulted), || Ok(()))
    }
}
