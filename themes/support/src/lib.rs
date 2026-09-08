//! Shared native projections and geometry helpers; no theme registration or RPC.
pub use weasel_theme_api as theme_api;
pub mod appearance;
pub mod bindings;
pub mod d2d_bindings;
pub mod presentation;
pub mod diagnostics {
    pub fn record(message: std::fmt::Arguments<'_>) {
        eprintln!("weasel-renderer: {message}");
    }
}
