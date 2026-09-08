//! Registration and ordered initialization fallback for built-in themes.
use crate::theme_api::ThemeFactory;

pub fn supports_theme(name: &str) -> bool {
    theme_candidates(name)
        .iter()
        .any(|factory| factory.name() == name)
}

/// Prefer the configured backend, retaining the other initialization fallback.
pub fn theme_candidates(preferred: &str) -> Vec<&'static dyn ThemeFactory> {
    // An invisible theme is opt-in, never a silent fallback for broken UI.
    if preferred == "void" {
        return vec![&crate::theme_void::Factory];
    }
    let mut themes: Vec<&'static dyn ThemeFactory> = vec![
        &crate::theme_eleven::Factory,
        &crate::theme_ten::Factory,
        &crate::theme_abc::Factory,
    ];
    themes.sort_by_key(|theme| theme.name() != preferred);
    themes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_preserves_fallback_without_creating_ui() {
        assert!(supports_theme("void"));
        let silent = theme_candidates("void");
        assert_eq!(silent.len(), 1);
        assert_eq!(silent[0].name(), "void");
        assert!(!silent[0].capabilities().preedit);
        for (preferred, expected) in [
            ("ten", ["ten", "eleven", "abc"]),
            ("eleven", ["eleven", "ten", "abc"]),
            ("abc", ["abc", "eleven", "ten"]),
            ("unknown", ["eleven", "ten", "abc"]),
        ] {
            let names: Vec<_> = theme_candidates(preferred)
                .iter()
                .map(|theme| theme.name())
                .collect();
            assert_eq!(names, expected);
            assert_eq!(supports_theme(preferred), preferred != "unknown");
            for factory in theme_candidates(preferred) {
                assert_eq!(factory.capabilities().preedit, factory.name() == "abc");
            }
        }
    }
}
