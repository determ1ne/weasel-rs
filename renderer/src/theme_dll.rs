//! Loader and host-side adapters. Successful DLL loads are intentionally pinned
//! until process exit: registered window procedures and XAML callbacks may
//! outlive a backend. Instances still close on their creating UI apartment.
use crate::d2d_bindings::*;
use crate::theme_api::*;
use std::{ffi::c_void, path::Path};
use weasel_theme_api::plugin::{self, Buffer, Create, Description, Operation, PluginApi, Reply};
use windows_strings::{PCSTR, PCWSTR};

pub struct Factory {
    name: &'static str,
    api: &'static PluginApi,
    description: Description,
}

struct BufferGuard<'a>(&'a PluginApi, Buffer);
impl Drop for BufferGuard<'_> {
    fn drop(&mut self) {
        let buffer = std::mem::replace(
            &mut self.1,
            Buffer {
                data: std::ptr::null_mut(),
                len: 0,
            },
        );
        unsafe {
            (self.0.release)(buffer);
        }
    }
}

fn read<T: serde::de::DeserializeOwned>(api: &PluginApi, buffer: Buffer) -> Result<T, String> {
    let buffer = BufferGuard(api, buffer);
    if buffer.1.data.is_null() || buffer.1.len == 0 || buffer.1.len > plugin::MAX_BYTES {
        return Err("theme returned an invalid payload".into());
    }
    serde_json::from_slice(unsafe { std::slice::from_raw_parts(buffer.1.data, buffer.1.len) })
        .map_err(|e| format!("invalid theme response: {e}"))
}

impl Factory {
    pub fn load(name: &'static str, path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let library = LoadLibraryExW(
                PCWSTR(wide.as_ptr()),
                None,
                (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) as u32,
            );
            if library.0.is_null() {
                return Err(format!(
                    "{}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            let loaded = (|| {
                let entry = GetProcAddress(library, PCSTR(plugin::ENTRY.as_ptr()))
                    .ok_or("theme entry point is missing")?;
                let entry: unsafe extern "C" fn() -> *const PluginApi = std::mem::transmute(entry);
                let api = entry().as_ref().ok_or("theme entry returned null")?;
                if api.size != std::mem::size_of::<PluginApi>() {
                    return Err("theme DLL must be updated together with renderer".into());
                }
                let description: Result<Description, String> = read(api, (api.describe)())?;
                let description = description?;
                if description.name != name || !description.defaults.is_object() {
                    return Err("theme metadata does not match its registration".into());
                }
                Ok(Self {
                    name,
                    api,
                    description,
                })
            })();
            if loaded.is_err() {
                let _ = FreeLibrary(library);
            }
            loaded
        }
    }
}

impl ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        self.name
    }
    fn capabilities(&self) -> ThemeCapabilities {
        self.description.capabilities
    }
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        Ok(self.description.defaults.clone())
    }
    fn create(
        &self,
        mode: UiMode,
        config: &weasel_common::settings::ConfigSnapshot,
    ) -> ThemeCreation {
        let mut notices = Vec::new();
        let result = (|| {
            let request = Create {
                mode,
                settings: config
                    .query(".")?
                    .cloned()
                    .ok_or("missing configuration root")?,
            };
            let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
            if bytes.len() > plugin::MAX_BYTES {
                return Err("theme settings payload too large".into());
            }
            let mut backend = Backend {
                api: self.api,
                handle: std::ptr::null_mut(),
                events: None,
                notices: Vec::new(),
                failure: None,
            };
            let reply: Reply = read(self.api, unsafe {
                (self.api.create)(bytes.as_ptr(), bytes.len(), &mut backend.handle)
            })?;
            notices = reply.notices;
            if let Some(error) = reply.error {
                return Err(error);
            }
            if backend.handle.is_null() {
                return Err("theme did not create an instance".into());
            }
            Ok(Box::new(backend) as Box<dyn ThemeBackend>)
        })();
        ThemeCreation {
            backend: result,
            notices,
        }
    }
}

struct Backend {
    api: &'static PluginApi,
    handle: *mut c_void,
    events: Option<(u64, EventSink)>,
    notices: Vec<ThemeNotice>,
    failure: Option<String>,
}

impl Backend {
    fn call(&mut self, operation: Operation) -> Result<(), String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let bytes = serde_json::to_vec(&operation).map_err(|e| e.to_string())?;
        if bytes.len() > plugin::MAX_BYTES {
            return Err("theme view payload too large".into());
        }
        let reply: Reply = read(self.api, unsafe {
            (self.api.invoke)(self.handle, bytes.as_ptr(), bytes.len())
        })?;
        self.notices.extend(reply.notices);
        if let Some(error) = reply.error {
            return Err(error);
        }
        if let Some((current, sink)) = &self.events {
            for (content, action) in reply.events {
                if content == *current {
                    sink.send(action);
                }
            }
        }
        Ok(())
    }
}

impl ThemeBackend for Backend {
    fn take_notices(&mut self) -> Vec<ThemeNotice> {
        std::mem::take(&mut self.notices)
    }
    fn render(&mut self, view: &CandidateView, events: &EventSink) -> Result<(), String> {
        self.events = Some((view.content_id, events.clone()));
        self.call(Operation::Render(view.clone()))
    }
    fn hide(&mut self) {
        self.events = None;
        if let Err(error) = self.call(Operation::Hide) {
            self.failure = Some(error);
        }
    }
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.call(Operation::Refresh)
    }
    fn check_health(&mut self) -> Result<(), String> {
        self.call(Operation::Health)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.api.destroy)(self.handle);
            }
        }
    }
}
