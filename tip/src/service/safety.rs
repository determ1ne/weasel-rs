//! 管理编辑调度预约和上下文级隔离；结果不确定的宿主写入绝不自动重放。
use super::*;

/// 编辑会话调度期间持有的预约；未成功移交时负责释放对应的忙碌标志。
pub(super) struct EditReservation<'a> {
    /// 拥有该预约的服务实例。
    service: &'a TextService,
    /// 创建预约时的请求票号。
    ticket: u64,
    /// 创建预约时的服务代次。
    generation: u64,
    /// 标记预约是否已交给 TSF 调度器。
    handed_off: bool,
}
impl<'a> EditReservation<'a> {
    /// 为指定请求票号和服务代次创建预约。
    pub fn new(service: &'a TextService, ticket: u64, generation: u64) -> Self {
        Self {
            service,
            ticket,
            generation,
            handed_off: false,
        }
    }
    /// 将预约所有权交给 TSF；析构时不再清除忙碌标志。
    pub fn hand_off(&mut self) {
        self.handed_off = true;
    }
}
impl Drop for EditReservation<'_> {
    /// 仅当票号和代次仍匹配时撤销未移交的预约，避免覆盖较新的请求。
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
    /// 遇到只读文档时使该上下文的待写入工作和响应失效。
    ///
    /// 保留宿主现有文本；发送取消命令后再次推进代次，确保取消响应也不能请求写会话。
    pub(super) fn reject_readonly_edit(&self, state: &ContextState) -> Result<()> {
        state.finishing_raw.store(false, Ordering::Release);
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
        let _ = self.lock(&state.rpc)?.context_command(command);
        // Existing host text is left untouched. Once writable, the normal
        // disconnected-composition path ends any remaining composition.
        Ok(())
    }

    /// 隔离发生本地编辑故障的上下文，并请求后续维护。
    ///
    /// 通过挂起标志和上下文代次拒绝该上下文的迟到响应，不影响共享 RPC 连接或其他上下文。
    pub(super) fn quarantine(&self, state: &ContextState, reason: &'static str, code: u64) {
        state.finishing_raw.store(false, Ordering::Release);
        if state.suspended.swap(true, Ordering::AcqRel) {
            return;
        }
        state.generation.fetch_add(1, Ordering::AcqRel);
        self.faulted.report_quarantine(state.id, reason, code);
        self.faulted.request_maintenance();
        // Generation/suspended reject this context's replies. Do not invalidate
        // the shared transport and unrelated contexts for a local edit failure.
    }

    /// 从待处理队列移除指定上下文的编辑工作，并保留其他上下文的队列顺序。
    ///
    /// 若丢弃了原始组合收尾任务，同时清除其状态标记，防止上下文永久停留在收尾中。
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
        if discarded
            .iter()
            .any(|task| matches!(task.step, EditStep::FinishRawComposition))
        {
            if let Some(state) = discarded.front().map(|task| &task.state) {
                state.finishing_raw.store(false, Ordering::Release);
            }
        }
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
