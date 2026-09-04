use std::{path::Path, ptr::NonNull};

use super::raw;

use crate::bindings::{
    FreeLibrary, GetProcAddress, HMODULE, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_strings::{HSTRING, PCWSTR, s};

pub struct RimeLibrary {
    module: HMODULE,
    api: NonNull<raw::RimeApi>,
}

impl RimeLibrary {
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(path)
            .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
        let wide = HSTRING::from(path.as_path());
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR::from_raw(wide.as_ptr()),
                Default::default(),
                (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) as u32,
            )
        };
        // Generated bindings return the raw handle, not a Result.
        if module.0.is_null() {
            return Err(format!(
                "LoadLibraryExW({}) failed: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }

        let symbol = unsafe { GetProcAddress(module, s!("rime_get_api")) };
        let Some(symbol) = symbol else {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(format!("rime_get_api was not found in {}", path.display()));
        };

        let get_api: unsafe extern "C" fn() -> *mut raw::RimeApi =
            unsafe { std::mem::transmute(symbol) };
        let api = unsafe { get_api() };
        let Some(api) = NonNull::new(api) else {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err("rime_get_api returned a null API pointer".to_owned());
        };

        Ok(Self { module, api })
    }

    pub fn api(&self) -> NonNull<raw::RimeApi> {
        self.api
    }
}

impl Drop for RimeLibrary {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}
