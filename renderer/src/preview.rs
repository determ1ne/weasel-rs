//! Standalone skin preview.
//!
//! `weasel-renderer.exe --preview` renders the configured skin from a synthetic
//! snapshot into a window that is visible in the task bar and manually closable.
//! It reads settings from broker, but never connects to server or listens on the
//! live renderer pipe. Candidate interactions are visual only.
use crate::{
    theme_api::UiMode,
    ui_runtime::{UiCommand, UiHandle},
};
use weasel_common::message::{RenderItem, RenderRect, RenderSnapshot};

/// Synthetic connection owner and presentation sequence for the preview strip.
const PREVIEW_OWNER: u64 = 1;

/// True when launched with `--preview`. No sub-arguments are accepted: the theme
/// is always sourced from the broker configuration.
pub fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<bool, String> {
    let mut preview = false;
    for arg in args {
        if arg != "--preview" || preview {
            return Err(format!(
                "unsupported renderer argument: {arg:?}; usage: weasel-renderer.exe [--preview]"
            ));
        }
        preview = true;
    }
    Ok(preview)
}

/// A representative candidate strip so the whole skin is visible: candidate rows
/// with comments plus the paging/emoji quick-action panel (both paging flags are
/// set and the strip is not on its first page).
pub fn synthetic_snapshot() -> RenderSnapshot {
    let samples = [
        ("你好", "nihao"),
        ("你好吗", "nihaoma"),
        ("你号码", "nihuoma"),
        ("你好呀", "nihaoya"),
        ("年号", "nianhao"),
        ("鸟窝", "niaowo"),
        ("拟好", "nihao"),
        ("你壕", "nihao"),
        ("女号", "nuhao"),
        ("你嚎", "nihao"),
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

pub fn run() -> Result<(), String> {
    // Run the bounded (2s) broker lookup on a throwaway runtime before the UI
    // thread starts, so the preview reflects the configured skin. refresh re-reads
    // the on-disk configuration so a just-edited setup is shown. The runtime is
    // dropped here; the UI thread uses only std channels and PostThreadMessage.
    let settings = {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|error| format!("could not create preview runtime: {error}"))?;
        runtime.block_on(crate::rpc::load_theme(true))?
    };

    let mut ui = UiHandle::start(&settings.theme, UiMode::Preview, &settings.theme_settings)?;
    // Preview never sends selections/page actions to the live engine.
    ui.events.close();
    ui.command_sender()
        .send(UiCommand::Render(PREVIEW_OWNER, synthetic_snapshot()))?;
    ui.wait_for_close()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_strict() {
        let parse = |args: &[&str]| parse_args(args.iter().map(std::ffi::OsString::from));
        assert!(!parse(&[]).unwrap());
        assert!(parse(&["--preview"]).unwrap());
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
