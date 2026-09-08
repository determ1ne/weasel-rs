//! void theme DLL. Native resources stay on their creating UI thread.
pub use weasel_theme_api as theme_api;
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
