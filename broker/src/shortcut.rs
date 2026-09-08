//! Installer-only shortcut creation. All COM objects remain on this STA thread.
use std::path::Path;

use windows_core::Interface;
use windows_strings::{HSTRING, w};

use crate::bindings::*;

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// Own the string exactly as InitPropVariantFromString does in the SDK helper.
// Do not clone: the generated PROPVARIANT itself has no ownership semantics.
struct StringProperty(PROPVARIANT);

impl StringProperty {
    fn new(text: &str) -> windows_core::Result<Self> {
        let mut value = PROPVARIANT::default();
        unsafe {
            let string = SHStrDupW(&HSTRING::from(text))?;
            value.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VARTYPE(VT_LPWSTR as u16),
                Anonymous: PROPVARIANT_0_0_0 { pwszVal: string },
                ..Default::default()
            });
        }
        Ok(Self(value))
    }
}

impl Drop for StringProperty {
    fn drop(&mut self) {
        unsafe {
            let _ = PropVariantClear(&mut self.0);
        }
    }
}

pub(crate) fn install(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err("shortcut path must be absolute".into());
    }
    let executable = std::env::current_exe()?;
    let directory = executable.parent().ok_or("broker directory unavailable")?;
    // Declared before the interfaces so COM is uninitialized last, including errors.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as _).ok()? };
    let _apartment = Apartment;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER as _)?;
        link.SetPath(&HSTRING::from(executable.as_os_str())).ok()?;
        link.SetArguments(w!("")).ok()?;
        link.SetWorkingDirectory(&HSTRING::from(directory.as_os_str()))
            .ok()?;
        link.SetDescription(w!("小狼毫RS算法服务")).ok()?;
        link.SetIconLocation(&HSTRING::from(executable.as_os_str()), 0)
            .ok()?;
        let properties: IPropertyStore = link.cast()?;
        let app_id = StringProperty::new(crate::toast::APP_ID)?;
        properties.SetValue(&PKEY_AppUserModel_ID, &app_id.0).ok()?;
        properties.Commit().ok()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(path.as_os_str()), true).ok()?;
    }
    Ok(())
}
