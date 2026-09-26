//! 管理 TSF 编辑会话中的组合文本生命周期。
//!
//! 本模块将异步引擎响应转换为针对指定 `ITfContext` 的编辑步骤，并在 TSF
//! 授予的编辑 Cookie 有效期间更新、提交或结束组合。编辑请求可能经由宿主
//! 回调重入，因此每一步都要重新核对上下文令牌和当前组合，不能把旧响应
//! 应用到已失效或已被替换的编辑状态。
use super::*;
use crate::bindings::TF_E_READONLY;

/// 描述预编辑文本从显示到提交的生命周期阶段。
#[derive(Default)]
enum CompositionPhase {
    /// 文本仍是可撤销的预编辑内容。
    #[default]
    Preedit,
    /// 正在调用宿主文本接口提交；此时不再保留可恢复的原始输入。
    Committing,
    /// 提交调用完成，等待 TSF 结束组合。
    Committed,
}

/// 与当前组合文本绑定的元数据，不随最新排队响应而转移。
///
/// 特别是 `raw` 只属于它所描述的预编辑文本；宿主在提交期间或提交后终止
/// 组合时，不得将旧原始输入重新解释为尚未提交的文本。
#[derive(Default)]
pub(super) struct CompositionContent {
    /// 原始输入是否仍可作为当前预编辑内容恢复。
    phase: CompositionPhase,
    /// 生成当前预编辑文本的原始输入；旧引擎响应缺少该信息时保持为空。
    raw: Option<String>,
}

impl CompositionContent {
    /// 创建一份属于新预编辑内容的元数据。
    pub(super) fn preedit(raw: Option<String>) -> Self {
        Self {
            phase: CompositionPhase::Preedit,
            raw,
        }
    }

    /// 开始向 TSF 写入提交文本，并立即放弃恢复原始输入的资格。
    ///
    /// `SetText` 可能重入宿主代码，也可能以失败结束；两种情况下提交结果
    /// 都可能不确定，因此调用后都不能把内容回滚为原始输入。
    fn begin_commit(&mut self) {
        // SetText can reenter the host. Never roll back an in-flight or
        // uncertain commit to raw input, including when SetText fails.
        self.phase = CompositionPhase::Committing;
        self.raw = None;
    }

    /// 结束当前组合元数据，并取出仍可恢复的预编辑原始输入。
    ///
    /// 只有尚未开始提交的预编辑内容会返回原始输入；提交中、已提交或缺少
    /// 原始输入时均返回 `None`。无论结果如何，本对象都会重置为默认状态。
    pub(super) fn finish(&mut self) -> Option<String> {
        let previous = std::mem::take(self);
        match previous.phase {
            CompositionPhase::Preedit => previous.raw,
            CompositionPhase::Committing | CompositionPhase::Committed => None,
        }
    }
}

impl TextService {
    /// 校验并排队一个针对指定 TSF 上下文的编辑步骤。
    ///
    /// 普通响应必须匹配上下文令牌；断连清理步骤例外，因为它负责处理已
    /// 失效连接留下的组合。队列有界，清理步骤优先于普通响应。此方法只请求
    /// TSF 编辑会话，不在调用线程直接修改宿主文本。
    pub(super) fn request_edit_session(
        &self,
        context: ITfContext,
        response: KeyEventResponse,
        step: EditStep,
        session: IUnknown,
    ) -> Result<()> {
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

    /// 从待处理队列取出仍有效的编辑，并向 TSF 请求异步可写编辑会话。
    ///
    /// 预留票据与服务代次用于识别请求期间发生的失效或重入。拒绝写入、
    /// 请求失败及会话失败分别执行恢复或隔离处理；成功时由编辑会话接管
    /// 预留状态。
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
            if matches!(pending.step, EditStep::FinishRawComposition) {
                pending.state.finishing_raw.store(false, Ordering::Release);
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
        let session: ITfEditSession =
            edit_session::ResponseEdit::new(self, pending, generation, ticket).into();
        let request = unsafe {
            context.RequestEditSession(tid, &session, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)
        };
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

    /// 在当前 TSF 编辑会话中建立组合范围并初始化预编辑状态。
    ///
    /// 通过上下文令牌确认响应仍属于当前会话后，使用 `ITfInsertAtSelection`
    /// 查询插入位置，再由 `ITfContextComposition` 建立组合。后续文本更新或
    /// 提交会排入独立编辑步骤，避免在同一宿主回调中混用旧响应。
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
        *self.lock(&state.composition_content)? = CompositionContent::preedit(
            response
                .commit_text
                .is_empty()
                .then(|| response.raw_input.clone())
                .flatten(),
        );
        state.composition_epoch.store(
            response.token.as_ref().map_or(0, |t| t.connection_epoch),
            Ordering::Release,
        );
        state.applied_layout_revision.store(0, Ordering::Release);
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

    /// 将引擎响应中的预编辑文本、光标和可选显示属性应用到现有组合。
    ///
    /// 只在上下文令牌有效且 TSF 组合仍存在时修改范围。显示属性与布局通知
    /// 属于尽力处理；它们失败时不应回放或否定已经成功的文本编辑。
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
        *self.lock(&state.composition_content)? =
            CompositionContent::preedit(response.raw_input.clone());
        self.set_range_text(&range, ec, &response.composition)?;
        // Display attributes are optional.  Continue the edit session when
        // the host does not expose a writable attribute property.
        self.set_display_attribute_best_effort(context, ec, &range);
        self.set_selection(context, ec, &range, response.composition_cursor as usize)?;
        *self.lock(&state.composition_text)? = response.composition.clone();
        *self.lock(&state.composition_cursor)? = response.composition_cursor as usize;
        state
            .applied_layout_revision
            .store(response.revision, Ordering::Release);
        // Geometry failure must not fault or replay a successful text edit.
        // Layout notifications can still supply the position later.
        let _ = self.request_composition_layout(&state);
        Ok(())
    }

    /// 在当前选择处插入非组合提交文本，并按响应要求继续建立组合。
    ///
    /// 插入由 TSF 编辑 Cookie 授权；插入后折叠范围到末尾并设置选择。若引擎
    /// 同时要求继续组合，则排队新的组合启动步骤。
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

    /// 将现有组合范围替换为提交文本，并排队结束组合步骤。
    ///
    /// 在清除显示属性、写入文本及调整选择前后反复核对令牌和组合对象，
    /// 因为 `SetText` 等宿主调用可能触发终止回调并使当前代次失效。开始
    /// 写入前先标记提交阶段，避免重入时恢复已经提交或状态不确定的原始输入。
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
        if !state.matches(response.token.as_ref())?
            || self.lock(&state.composition)?.as_ref() != Some(&active)
        {
            return Ok(());
        }
        self.lock(&state.composition_content)?.begin_commit();
        self.set_range_text(&range, ec, &response.commit_text)?;
        // A host termination callback may already have taken this composition
        // and invalidated its generation while SetText was executing.
        if !state.matches(response.token.as_ref())?
            || self.lock(&state.composition)?.as_ref() != Some(&active)
        {
            return Ok(());
        }
        self.lock(&state.composition_content)?.phase = CompositionPhase::Committed;
        self.collapse_end(&range, ec)?;
        self.set_selection(context, ec, &range, 0)?;
        if !state.matches(response.token.as_ref())?
            || self.lock(&state.composition)?.as_ref() != Some(&active)
        {
            return Ok(());
        }
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

    /// 结束当前 TSF 组合，可选清除范围，并按需排队后续组合。
    ///
    /// 先从上下文状态中取出组合，令重入的宿主回调无法再次操作同一组合；
    /// 清理显示属性、文本及保存的宿主选择后调用 `EndComposition`。恢复选择
    /// 只适用于仍保存的有效选择，重启则作为新的编辑请求处理。
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
        let _ = self.lock(&state.composition_content)?.finish();
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

    /// 在 TSF 授予的编辑 Cookie 下执行一个已排队步骤。
    ///
    /// `ApplyResponse` 会根据提交文本、组合状态及响应标志展开为具体步骤。
    /// 断连步骤保留现有预编辑文本并结束 TSF 组合，不采纳候选、不删除宿主
    /// 文本，也不恢复可能过期的选择。调用的 TSF 接口可能触发宿主回调，故
    /// 具体操作仍须自行复查令牌与组合状态。
    pub(super) fn apply_edit(&self, pending: PendingEdit, ec: TfEditCookie) -> Result<()> {
        let PendingEdit {
            state,
            context,
            response,
            step,
            session,
        } = pending;
        match step {
            EditStep::FinishRawComposition => {
                let result = self.finish_raw_composition(&state, ec, false);
                state.finishing_raw.store(false, Ordering::Release);
                result
            }
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
                    let _ = self.lock(&state.composition_content)?.finish();
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

#[cfg(test)]
mod content_tests {
    use super::{CompositionContent, CompositionPhase};

    #[test]
    fn termination_preserves_inflight_and_completed_commits() {
        for completed in [false, true] {
            let mut content = CompositionContent::preedit(Some("nihao".into()));
            content.begin_commit();
            if completed {
                content.phase = CompositionPhase::Committed;
            }
            // Both host termination and the click-outside task use finish().
            assert_eq!(content.finish(), None);
            assert_eq!(content.finish(), None);
        }
    }

    #[test]
    fn new_preedit_after_commit_has_its_own_raw_input() {
        let mut content = CompositionContent::preedit(Some("nihao".into()));
        assert_eq!(content.finish().as_deref(), Some("nihao"));
        content = CompositionContent::preedit(Some("ni".into()));
        content.begin_commit();
        content.phase = CompositionPhase::Committed;
        assert_eq!(content.finish(), None);
        content = CompositionContent::preedit(Some("hao".into()));
        assert_eq!(content.finish().as_deref(), Some("hao"));
        // Older servers without raw_input must not synthesize any text.
        assert_eq!(CompositionContent::preedit(None).finish(), None);
    }
}
