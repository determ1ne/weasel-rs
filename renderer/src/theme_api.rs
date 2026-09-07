//! Theme contract: factories are shared; backend resources stay on the UI thread.
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeCapabilities {
    /// Whether this backend can display preedit outside the host application.
    pub preedit: bool,
}

impl ThemeCapabilities {
    pub const CANDIDATES_ONLY: Self = Self { preedit: false };
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Anchor {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub valid: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandidateItem {
    pub primary_text: String,
    pub secondary_text: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandidateView {
    /// None means inline input: show candidates without an input field.
    pub preedit: Option<Preedit>,
    /// Opaque presentation identity. Changes on content or routing changes,
    /// but not on geometry-only updates. Not an RPC revision or session ID.
    pub content_id: u64,
    pub visible: bool,
    pub anchor: Option<Anchor>,
    pub items: Vec<CandidateItem>,
    pub selected_index: u32,
    pub page_start: u32,
    pub total_item_count: Option<u32>,
    pub can_page_previous: bool,
    pub can_page_next: bool,
}

pub fn same_content(a: &CandidateView, b: &CandidateView) -> bool {
    a.content_id == b.content_id
        && a.preedit == b.preedit
        && a.items == b.items
        && a.selected_index == b.selected_index
        && a.page_start == b.page_start
        && a.total_item_count == b.total_item_count
        && a.can_page_previous == b.can_page_previous
        && a.can_page_next == b.can_page_next
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preedit {
    pub text: String,
    /// UTF-16 code-unit offset, on a Unicode scalar boundary.
    pub cursor: u32,
}

impl Preedit {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > 65536 || self.text.contains('\0') {
            return Err("invalid preedit text".into());
        }
        let mut offset = 0;
        for ch in self.text.chars() {
            if offset == self.cursor {
                return Ok(());
            }
            offset += ch.len_utf16() as u32;
        }
        if offset == self.cursor {
            Ok(())
        } else {
            Err("invalid preedit cursor".into())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiAction {
    ItemInvoked(u32),
    NavigatePrevious,
    NavigateNext,
    OpenEmojiPanel,
}

/// Bound to the identity of the displayed frame, never to mutable latest state.
#[derive(Clone)]
pub struct EventSink(Arc<dyn Fn(UiAction) + Send + Sync>);

impl EventSink {
    pub(crate) fn new(callback: impl Fn(UiAction) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }

    pub fn send(&self, action: UiAction) {
        (self.0)(action);
    }
}

#[cfg(windows)]
pub trait ThemeBackend {
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String>;
    fn hide(&mut self);
    /// Invalidate appearance resources only. The runtime decides whether the
    /// current owner still permits redrawing its snapshot.
    fn refresh_appearance(&mut self) -> Result<(), String>;
    fn pre_translate(
        &mut self,
        _message: &crate::bindings::Windows::Win32::MSG,
    ) -> Result<bool, String> {
        Ok(false)
    }
    fn check_health(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Metadata queries must not create native resources. Only the UI apartment
/// calls create; the returned backend is deliberately not required to be Send.
#[cfg(windows)]
pub trait ThemeFactory: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ThemeCapabilities;
    /// Theme-local defaults, overlaid by installation/user settings before create.
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::Value::Object(Default::default()))
    }
    /// Local immutable configuration; each theme interprets its own settings.
    fn create(
        &self,
        mode: UiMode,
        settings: &weasel_common::settings::ConfigSnapshot,
    ) -> Result<Box<dyn ThemeBackend>, String>;
}

/// How the renderer is running. Live is the normal per-candidate strip driven
/// by the server; Preview is a standalone, closable stand-in showing the skin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiMode {
    Live,
    Preview,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preedit_cursor_uses_utf16_boundaries() {
        for (cursor, valid) in [
            (0, true),
            (1, true),
            (2, false),
            (3, true),
            (4, true),
            (5, false),
        ] {
            assert_eq!(
                Preedit {
                    text: "a😀中".into(),
                    cursor
                }
                .validate()
                .is_ok(),
                valid
            );
        }
        assert!(
            Preedit {
                text: String::new(),
                cursor: 0
            }
            .validate()
            .is_ok()
        );
        assert!(
            Preedit {
                text: "\0".into(),
                cursor: 0
            }
            .validate()
            .is_err()
        );
    }
}
