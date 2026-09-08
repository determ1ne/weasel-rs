//! System appearance is independent of the selected drawing toolkit.
use crate::bindings::{UIColorType, UISettings};

pub fn is_dark() -> bool {
    UISettings::new()
        .and_then(|settings| settings.GetColorValue(UIColorType::Background))
        .is_ok_and(|color| is_dark_rgb(color.R, color.G, color.B))
}

fn is_dark_rgb(r: u8, g: u8, b: u8) -> bool {
    299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000
}

pub struct AppearanceSubscription {
    // Revoke the callback before releasing settings.
    _revoker: Option<windows_core::EventRevoker>,
    _settings: UISettings,
}

impl AppearanceSubscription {
    pub fn new(thread_id: u32, message: u32) -> Option<Self> {
        use crate::bindings::{LPARAM, PostThreadMessageW, WPARAM};
        let settings = UISettings::new().ok()?;
        let revoker = settings
            .ColorValuesChanged(move |_, _| unsafe {
                let _ = PostThreadMessageW(thread_id, message, WPARAM(0), LPARAM(0));
            })
            .ok();
        Some(Self {
            _revoker: revoker,
            _settings: settings,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn classifies_system_background() {
        assert!(super::is_dark_rgb(28, 28, 28));
        assert!(!super::is_dark_rgb(248, 248, 248));
    }
}
