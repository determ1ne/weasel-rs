//! Known theme DLLs and ordered initialization fallback. Missing optional DLLs
//! are skipped; void is opt-in and never a fallback for a broken visible theme.
use crate::theme_api::ThemeFactory;
use std::sync::OnceLock;

pub fn supports_theme(name: &str) -> bool {
    ["eleven", "ten", "abc", "void"].contains(&name)
}

fn names(preferred: &str) -> Vec<&'static str> {
    if preferred == "void" {
        return vec!["void"];
    }
    let mut names = vec!["eleven", "ten", "abc"];
    names.sort_by_key(|name| *name != preferred);
    names
}

pub fn theme_candidates(preferred: &str) -> Vec<&'static dyn ThemeFactory> {
    static ELEVEN: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static TEN: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static ABC: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static VOID: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    names(preferred)
        .into_iter()
        .filter_map(|name| {
            let slot = match name {
                "eleven" => &ELEVEN,
                "ten" => &TEN,
                "abc" => &ABC,
                _ => &VOID,
            };
            let loaded = slot.get_or_init(|| {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let directory = exe.parent().ok_or("renderer has no executable directory")?;
                let filename = format!("weasel_theme_{name}.dll");
                let installed = directory.join("themes").join(&filename);
                // Cargo emits cdylibs beside EXEs. Only use that layout inside a
                // Cargo output directory; installed builds load themes/ exclusively.
                let cargo_output = directory.join("deps").is_dir();
                let path = if !installed.exists() && cargo_output {
                    directory.join(filename)
                } else {
                    installed
                };
                crate::theme_dll::Factory::load(name, &path)
            });
            match loaded {
                Ok(factory) => Some(factory as &'static dyn ThemeFactory),
                Err(error) => {
                    if name == preferred || name == "ten" {
                        crate::diagnostics::record(format_args!(
                            "theme {name} unavailable: {error}"
                        ));
                    }
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn preference_preserves_visible_fallback_and_void_is_opt_in() {
        assert_eq!(super::names("abc"), ["abc", "eleven", "ten"]);
        assert_eq!(super::names("unknown"), ["eleven", "ten", "abc"]);
        assert_eq!(super::names("void"), ["void"]);
    }
}
