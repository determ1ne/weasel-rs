//! UI-thread-only rendering boundary. Concrete XAML/D2D resources stay private.
use crate::{bindings::Windows::Win32::MSG, ui_runtime::EventSender};
use weasel_common::message::RenderSnapshot;

pub trait ThemeBackend {
    fn render(&mut self, snapshot: &RenderSnapshot, events: &EventSender) -> Result<(), String>;
    fn hide(&mut self);
    /// Invalidate appearance resources only. The runtime decides whether the
    /// current owner still permits redrawing its snapshot.
    fn refresh_appearance(&mut self) -> Result<(), String>;
    fn pre_translate(&mut self, _message: &MSG) -> Result<bool, String> {
        Ok(false)
    }
    fn check_health(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct ThemeRegistration {
    pub name: &'static str,
    pub create: fn() -> Result<Box<dyn ThemeBackend>, String>,
}

/// Prefer the configured backend, retaining the other initialization fallback.
pub fn theme_candidates(preferred: &str) -> Vec<ThemeRegistration> {
    let mut themes = vec![
        ThemeRegistration {
            name: "eleven",
            create: crate::theme_eleven::create,
        },
        ThemeRegistration {
            name: "ten",
            create: crate::theme_ten::create,
        },
    ];
    themes.sort_by_key(|theme| theme.name != preferred);
    themes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_preserves_fallback_without_creating_ui() {
        for (preferred, expected) in [
            ("ten", ["ten", "eleven"]),
            ("eleven", ["eleven", "ten"]),
            ("unknown", ["eleven", "ten"]),
        ] {
            let names: Vec<_> = theme_candidates(preferred)
                .iter()
                .map(|theme| theme.name)
                .collect();
            assert_eq!(names, expected);
        }
    }
}
