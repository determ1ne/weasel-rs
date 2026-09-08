//! Win10/11 desktop Toasts. No activation callbacks or elevated helper process.
//! The caller owns rate limiting; disabled OS notifications are not an error to
//! recover from by displaying a modal dialog. API failures are returned to the
//! notification center, which provides the portable-build MessageBox fallback.
use crate::bindings::*;
use windows_strings::HSTRING;

pub(crate) const APP_ID: &str = "WeaselRS.Broker";
// The installer assigns this identity to its existing Start Menu shortcut.
// Sending notifications must not create or modify shortcuts.

pub fn show(title: &str, message: &str) -> Result<(), String> {
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                RoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text><text>详情请查看 broker-notifications.*.log。</text></binding></visual><audio silent=\"true\"/></toast>",
        escape_xml(title),
        escape_xml(message)
    );
    let document = XmlDocument::new().map_err(|e| e.to_string())?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|e| e.to_string())?;
    let notification =
        ToastNotification::CreateToastNotification(&document).map_err(|e| e.to_string())?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(|e| e.to_string())?;
    notifier.Show(&notification).map_err(|e| e.to_string())
}

fn escape_xml(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
