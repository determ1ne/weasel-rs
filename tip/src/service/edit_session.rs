//! 执行提交给 TSF 的单个编辑任务，并隔离停用、重新激活前后的异步回调。
//!
//! 每个会话持有自己的待处理任务、票据和服务代次；迟到或重复的 COM 回调不能
//! 消费其他请求，也不能在新一轮激活中使用旧上下文或编辑 cookie。
use super::*;

#[implement(ITfEditSession)]
/// 一个不可变请求对应的 TSF 编辑会话；任务至多消费一次。
pub(super) struct ResponseEdit {
    /// 由 `_owner` 保活的文本服务对象地址，仅在回调期间解引用。
    service: *const TextService,
    /// 本会话专属任务；`DoEditSession` 从中取走任务以防重复执行。
    pending: Mutex<Option<PendingEdit>>,
    /// 创建本会话时的服务激活代次。
    generation: u64,
    /// 调度器为此请求分配的编辑票据。
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

impl Drop for ResponseEdit {
    /// TSF 丢弃未执行的会话时释放对应预约标记并请求维护。
    fn drop(&mut self) {
        if let Some(pending) = self
            .pending
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let service = unsafe { &*self.service };
            let _reservation = safety::EditReservation::new(service, self.ticket, self.generation);
            if matches!(pending.step, EditStep::DisconnectComposition) {
                pending
                    .state
                    .disconnect_requested
                    .store(false, Ordering::Release);
            }
            if matches!(pending.step, EditStep::FinishRawComposition) {
                pending.state.finishing_raw.store(false, Ordering::Release);
            }
            service.faulted.request_maintenance();
        }
    }
}

impl ResponseEdit {
    /// 构造会话并持有请求的 COM 所有者，使服务指针在回调和清理期间有效。
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
    /// 在 TSF 授予的写 cookie 中验证请求、协调组合状态并应用编辑。
    /// 回调不递归请求另一写锁；后续调度通过窗口消息恢复。若写入失败且文档
    /// 可能已被部分修改，则隔离后续输入，避免重放同一响应。
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        // TSF dispatches this session on the requesting apartment; _owner keeps
        // the pointed-to COM allocation alive throughout this callback.
        let service = unsafe { &*self.service };
        let _reservation = safety::EditReservation::new(service, self.ticket, self.generation);
        let result = boundary::guard(Some(&service.faulted), || {
            if !service.activated.load(Ordering::Acquire)
                || service.generation.load(Ordering::Acquire) != self.generation
                || service.edit_ticket.load(Ordering::Acquire) != self.ticket
            {
                return Ok(());
            }
            let pending = service.lock(&self.pending)?.take();
            let Some(pending) = pending else {
                return Ok(());
            };
            let state = pending.state.clone();
            struct CleanupFlag<'a>(Option<&'a AtomicBool>);
            impl Drop for CleanupFlag<'_> {
                fn drop(&mut self) {
                    if let Some(flag) = self.0 {
                        flag.store(false, Ordering::Release);
                    }
                }
            }
            let _cleanup_flag = CleanupFlag(
                matches!(pending.step, EditStep::DisconnectComposition)
                    .then_some(&state.disconnect_requested),
            );
            let _finish_flag = CleanupFlag(
                matches!(pending.step, EditStep::FinishRawComposition)
                    .then_some(&state.finishing_raw),
            );
            let valid = match pending.matches() {
                Ok(valid) => valid,
                Err(error) => {
                    service.quarantine(
                        &state,
                        "edit.validate_failed",
                        error.code().0 as u32 as u64,
                    );
                    return Err(error);
                }
            };
            let result = if valid {
                service.edit_mutated.store(false, Ordering::Release);
                state.editing.store(true, Ordering::Release);
                struct Editing<'a>(&'a AtomicBool);
                impl Drop for Editing<'_> {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _editing = Editing(&state.editing);
                let reconcile = if matches!(
                    pending.step,
                    EditStep::ApplyResponse
                        | EditStep::UpdateComposition
                        | EditStep::CommitComposition
                ) {
                    service.reconcile_before_edit(&state, ec)
                } else {
                    Ok(false)
                };
                match reconcile {
                    Ok(true) => Ok(()),
                    Ok(false) => service.apply_edit(pending, ec),
                    Err(error) => Err(error),
                }
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
                if error.code().0 == bindings::TF_E_READONLY
                    && !service.edit_mutated.load(Ordering::Acquire)
                {
                    service.reject_readonly_edit(&state)?;
                    return Ok(());
                }
                // A failed write may already have changed the document. Do not
                // replay the response or continue with dependent edit steps.
                if service.edit_mutated.load(Ordering::Acquire) {
                    service
                        .faulted
                        .mark("edit.write_uncertain", error.code().0 as u32 as u64);
                } else {
                    service.quarantine(
                        &state,
                        "edit.before_write_failed",
                        error.code().0 as u32 as u64,
                    );
                }
                if service.faulted.load(Ordering::Acquire) {
                    let discarded = std::mem::take(&mut *service.lock(&service.pending_edit)?);
                    drop(discarded);
                } else {
                    service.discard_context_edits(state.id)?;
                }
                service.lock(&service.rpc)?.log(
                    "error",
                    format!("edit failed; input quarantined: {error:?}"),
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
                        service.faulted.request_maintenance();
                        return Err(error);
                    }
                }
            }
            result
        });
        result
    }
}
