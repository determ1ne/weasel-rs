//! Shared Windows identity and local-object security. No registration or UI.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

pub(crate) fn validate_component(component: &str) -> std::io::Result<()> {
    if component.is_empty()
        || component.len() > 64
        || !component
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "component must be 1..64 ASCII letters, digits, hyphens or underscores",
        ));
    }
    Ok(())
}
