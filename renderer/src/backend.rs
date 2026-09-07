//! Registration and ordered initialization fallback for built-in themes.
use crate::theme_api::ThemeFactory;

/// Prefer the configured backend, retaining the other initialization fallback.
pub fn theme_candidates(preferred: &str) -> Vec<&'static dyn ThemeFactory> {
    let mut themes: Vec<&'static dyn ThemeFactory> =
        vec![&crate::theme_eleven::Factory, &crate::theme_ten::Factory];
    themes.sort_by_key(|theme| theme.name() != preferred);
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
                .map(|theme| theme.name())
                .collect();
            assert_eq!(names, expected);
            for factory in theme_candidates(preferred) {
                assert!(!factory.capabilities().preedit);
            }
        }
    }
}
