//! eleven theme DLL. Native resources stay on their creating UI thread.
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, diagnostics, presentation};
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
