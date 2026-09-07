//! Standalone skin preview.
//!
//! `weasel-renderer.exe --preview` renders the configured skin from a synthetic
//! snapshot into a window that is visible in the task bar and manually closable.
//! It reads settings from broker, but never connects to server or listens on the
//! live renderer pipe. Candidate interactions are visual only.
use crate::theme_api::UiMode;
use weasel_common::message::{RenderItem, RenderRect, RenderSnapshot};

/// Synthetic connection owner and presentation sequence for the preview strip.
pub(crate) const PREVIEW_OWNER: u64 = 1;

/// Theme selection comes from broker; preview has no input-mode overrides.
pub fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<UiMode, String> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(UiMode::Live),
        [arg] if arg == "--preview" => Ok(UiMode::Preview),
        _ => Err("usage: weasel-renderer.exe [--preview]".into()),
    }
}

/// A representative candidate strip so the whole skin is visible: candidate rows
/// with comments plus the paging/emoji quick-action panel (both paging flags are
/// set and the strip is not on its first page).
pub fn synthetic_snapshot() -> RenderSnapshot {
    let samples = [
        ("你好", "nihao"),
        ("小狼毫", "xiaolanghao"),
        ("Rime", ""),
        ("中州韻", ""),
    ];
    let items = samples
        .into_iter()
        .map(|(primary, secondary)| RenderItem {
            primary_text: primary.into(),
            secondary_text: secondary.into(),
            enabled: true,
            kind: "candidate".into(),
        })
        .collect();
    RenderSnapshot {
        visible: true,
        sequence: 1,
        session_id: 1,
        revision: 1,
        // The preview positions itself on the primary monitor and ignores the
        // anchor; it only needs to be valid to satisfy the visibility rule.
        anchor: Some(RenderRect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
            valid: true,
        }),
        items,
        selected_index: 0,
        page_start: 10,
        total_item_count: Some(30),
        can_page_previous: true,
        can_page_next: true,
        token: None,
    }
}

pub fn run(_mode: UiMode) -> Result<(), String> {
    crate::preview_window::run()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_strict() {
        let parse = |args: &[&str]| parse_args(args.iter().map(std::ffi::OsString::from));
        assert_eq!(parse(&[]).unwrap(), UiMode::Live);
        assert_eq!(parse(&["--preview"]).unwrap(), UiMode::Preview);
        assert!(parse(&["--preview", "--preedit"]).is_err());
        assert!(parse(&["--preedit"]).is_err());
        assert!(parse(&["--preview", "--preview"]).is_err());
        assert!(parse(&["--preveiw"]).is_err());
        assert!(parse(&["--preview", "ten"]).is_err());
    }

    #[test]
    fn synthetic_snapshot_is_valid_and_visible() {
        let snapshot = synthetic_snapshot();
        crate::state::validate(&snapshot).unwrap();
        assert!(crate::presentation::is_visible(
            &crate::theme_adapter::view(&snapshot, 1)
        ));
        assert!(snapshot.can_page_previous && snapshot.can_page_next);
        assert_eq!(snapshot.selected_index, 0);
        assert!((snapshot.selected_index as usize) < snapshot.items.len());
    }
}
