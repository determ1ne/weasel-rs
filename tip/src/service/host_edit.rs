//! Reconcile host-owned edits without asking for a write lock from OnEndEdit.
use super::*;
use weasel_common::message::ContextAction;

impl TextService {
    pub(super) fn on_host_edit(
        &self,
        context: Ref<'_, ITfContext>,
        ec: TfEditCookie,
        record: Ref<'_, ITfEditRecord>,
    ) -> Result<()> {
        let Some(context) = context.to_owned() else {
            return Ok(());
        };
        let Some(state) = self.find_context(&context)? else {
            return Ok(());
        };
        // Start/Update/End are separate write sessions. Do not interpret the
        // temporary empty range between our own steps as a host cancellation.
        if state.suspended.load(Ordering::Acquire)
            || state.editing.load(Ordering::Acquire)
            || state.reconciling.load(Ordering::Acquire)
            || self.edit_requested.load(Ordering::Acquire)
            || self
                .lock(&self.pending_edit)?
                .iter()
                .any(|task| task.state.id == state.id)
        {
            return Ok(());
        }
        let Some(composition) = self.lock(&state.composition)?.clone() else {
            return Ok(());
        };
        let range = unsafe { composition.GetRange()? };
        if unsafe { range.IsEmpty(ec)? }.as_bool() {
            return self.send_context_action(&state, ContextAction::Cancel, true);
        }
        let Some(record) = record.to_owned() else {
            return Ok(());
        };
        if !unsafe { record.GetSelectionStatus()? }.as_bool() {
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
                let previous = self
                    .lock(&state.host_selection)?
                    .replace((selected, selection.style));
                drop(previous);
                self.send_context_action(&state, ContextAction::Submit, true)?;
            }
        }
        Ok(())
    }
}
