//! 协调宿主直接对文本存储所做的编辑，并维护本 TIP 的组合状态。
//!
//! `OnEndEdit` 期间只检查宿主变化、失效旧响应并投递本地任务；需要写入或结束
//! 组合的操作留到后续 TSF 写编辑会话，避免在宿主回调中再次请求写锁。
use super::*;
use weasel_common::message::ContextAction;

impl TextService {
    /// 处理宿主编辑通知：刷新选区相关安全状态，识别宿主删除预编辑文本，
    /// 并把组合外点击转换为稍后执行的本地收尾任务。
    pub(super) fn on_host_edit(
        &self,
        context: Ref<'_, ITfContext>,
        ec: TfEditCookie,
        record: Ref<'_, ITfEditRecord>,
        owner: IUnknown,
    ) -> Result<()> {
        let Some(context) = context.to_owned() else {
            return Ok(());
        };
        let Some(state) = self.find_context(&context)? else {
            return Ok(());
        };
        let record = record.to_owned();
        let selection_changed = record
            .as_ref()
            .map(|record| unsafe { record.GetSelectionStatus() })
            .transpose()?
            .is_some_and(|changed| changed.as_bool());
        if selection_changed {
            self.refresh_secure_field_from_cookie(&state, ec);
        }
        // Start/Update/End are separate write sessions. Do not interpret the
        // temporary empty range between our own steps as a host cancellation.
        if state.suspended.load(Ordering::Acquire)
            || state.editing.load(Ordering::Acquire)
            || state.reconciling.load(Ordering::Acquire)
            || state.finishing_raw.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(composition) = self.lock(&state.composition)?.clone() else {
            return Ok(());
        };
        let range = unsafe { composition.GetRange()? };
        let range_empty = unsafe { range.IsEmpty(ec)? }.as_bool();
        // External preedit intentionally leaves an empty host range. Only a
        // previously nonempty inline preedit becoming empty means host deletion.
        // Drop the state lock before invoking TSF or sending a context command.
        let expected_empty = self.lock(&state.composition_text)?.is_empty();
        let edit_pending = self.edit_requested.load(Ordering::Acquire)
            || self
                .lock(&self.pending_edit)?
                .iter()
                .any(|task| task.state.id == state.id);
        if host_deleted_preedit(range_empty, expected_empty) && !edit_pending {
            return self.send_context_action(&state, ContextAction::Cancel, true);
        }
        let Some(_record) = record else {
            return Ok(());
        };
        if !selection_changed {
            return Ok(());
        }
        let mut selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(None),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_NONE,
                fInterimChar: BOOL(0),
            },
        };
        let mut fetched = 0;
        let result = unsafe {
            context.GetSelection(
                ec,
                bindings::TF_DEFAULT_SELECTION,
                1,
                &mut selection,
                &mut fetched,
            )
        };
        // GetSelection transfers a reference to the caller, even if later
        // checks fail. Take it out of the ABI wrapper immediately.
        let selected = unsafe { std::mem::ManuallyDrop::take(&mut selection.range) };
        result.ok()?;
        if fetched != 1 {
            return Ok(());
        }
        if let Some(selected) = selected {
            let before = unsafe { selected.CompareStart(ec, &range, TF_ANCHOR_START)? } < 0;
            let after = unsafe { selected.CompareEnd(ec, &range, TF_ANCHOR_END)? } > 0;
            if before || after {
                // Invalidate old replies now, but never request a write lock
                // from OnEndEdit. The update window drains this local task.
                self.send_context_action(&state, ContextAction::Cancel, true)?;
                self.discard_context_edits(state.id)?;
                if state.suspended.load(Ordering::Acquire) {
                    return Ok(());
                }
                state.finishing_raw.store(true, Ordering::Release);
                self.lock(&self.pending_edit)?.push_front(PendingEdit {
                    state: state.clone(),
                    context: context.clone(),
                    response: KeyEventResponse {
                        token: Some(state.token()?),
                        ..Default::default()
                    },
                    step: EditStep::FinishRawComposition,
                    session: owner,
                });
                let hwnd = self
                    .lock(&self.update_window)?
                    .as_ref()
                    .map(|window| window.hwnd);
                let posted = hwnd.is_some_and(|hwnd| unsafe {
                    bindings::PostMessageW(
                        Some(hwnd),
                        update_window::UPDATE_MESSAGE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                    .as_bool()
                });
                if !posted {
                    self.quarantine(&state, "composition.finish_post_failed", 0);
                    self.discard_context_edits(state.id)?;
                }
            }
        }
        Ok(())
    }

    /// 在写 cookie 下结束原始组合，可由本地任务或宿主终止回调调用。
    /// 恢复文本时始终使用原组合范围，不在可能已移动的当前插入点重新插入。
    pub(super) fn finish_raw_composition(
        &self,
        state: &ContextState,
        ec: TfEditCookie,
        host_terminated: bool,
    ) -> Result<()> {
        let active = self.lock(&state.composition)?.take();
        let Some(active) = active else {
            return Ok(());
        };
        let raw = self.lock(&state.composition_content)?.finish();
        let expected_empty = self.lock(&state.composition_text)?.is_empty();
        let range = unsafe { active.GetRange()? };
        let mut selection = TF_SELECTION::default();
        let mut fetched = 0;
        let result = unsafe {
            state.context.GetSelection(
                ec,
                bindings::TF_DEFAULT_SELECTION,
                1,
                &mut selection,
                &mut fetched,
            )
        };
        let selected = unsafe { std::mem::ManuallyDrop::take(&mut selection.range) };
        // If the host deleted inline text, do not resurrect it. Older servers
        // have no raw_input; preserve their text instead of guessing encoding.
        let deleted = host_deleted_preedit(unsafe { range.IsEmpty(ec)? }.as_bool(), expected_empty);
        self.clear_display_attribute_best_effort(&state.context, ec, &range);
        if !deleted {
            if let Some(raw) = raw {
                self.set_range_text(&range, ec, &raw)?;
            }
        }
        if !host_terminated {
            self.edit_mutated.store(true, Ordering::Release);
            unsafe {
                active.EndComposition(ec).ok()?;
            }
        }
        self.lock(&state.composition_text)?.clear();
        *self.lock(&state.composition_cursor)? = 0;
        self.lock(&state.host_selection)?.take();
        state.finishing_raw.store(false, Ordering::Release);
        if result.is_ok() && fetched == 1 {
            if let Some(selected) = selected {
                self.restore_selection(&state.context, ec, selected, selection.style)?;
            }
        }
        Ok(())
    }

    /// 在真正应用服务器响应前、持有写 cookie 时重新检查选区。
    /// 若排队后用户已点击到组合外，则先结束组合并取消响应，点击优先于旧回复。
    pub(super) fn reconcile_before_edit(
        &self,
        state: &ContextState,
        ec: TfEditCookie,
    ) -> Result<bool> {
        let Some(active) = self.lock(&state.composition)?.clone() else {
            return Ok(false);
        };
        let range = unsafe { active.GetRange()? };
        let mut selection = TF_SELECTION::default();
        let mut fetched = 0;
        let result = unsafe {
            state.context.GetSelection(
                ec,
                bindings::TF_DEFAULT_SELECTION,
                1,
                &mut selection,
                &mut fetched,
            )
        };
        let selected = unsafe { std::mem::ManuallyDrop::take(&mut selection.range) };
        result.ok()?;
        if fetched == 1 {
            if let Some(selected) = selected {
                let outside = unsafe {
                    selected.CompareStart(ec, &range, TF_ANCHOR_START)? < 0
                        || selected.CompareEnd(ec, &range, TF_ANCHOR_END)? > 0
                };
                if outside {
                    self.finish_raw_composition(state, ec, false)?;
                    self.send_context_action(state, ContextAction::Cancel, true)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// 仅当原本非空的内联预编辑范围被宿主清空时，才视为宿主删除了预编辑文本。
fn host_deleted_preedit(range_empty: bool, expected_empty: bool) -> bool {
    range_empty && !expected_empty
}

#[cfg(test)]
mod tests {
    use super::host_deleted_preedit;

    #[test]
    fn empty_external_preedit_is_not_host_deletion() {
        assert!(!host_deleted_preedit(true, true));
        assert!(host_deleted_preedit(true, false));
        assert!(!host_deleted_preedit(false, false));
        assert!(!host_deleted_preedit(false, true));
    }
}
