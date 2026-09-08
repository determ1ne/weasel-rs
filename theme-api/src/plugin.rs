//! Same-build DLL bridge. Rust objects and allocations never change allocator.
//! Entry points catch Rust panics; native window callbacks must also contain
//! panics themselves. This is not isolation from native faults or deadlocks.
use crate::*;
use serde::de::DeserializeOwned;
use std::{
    collections::VecDeque,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const ENTRY: &[u8] = b"weasel_theme_entry\0";

/// The producer owns this buffer; only its release entry point may free it.
#[repr(C)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
}

#[repr(C)]
pub struct PluginApi {
    pub size: usize,
    pub describe: unsafe extern "C" fn() -> Buffer,
    pub create: unsafe extern "C" fn(*const u8, usize, *mut *mut c_void) -> Buffer,
    pub invoke: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> Buffer,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub release: unsafe extern "C" fn(Buffer),
}

#[derive(Serialize, Deserialize)]
pub struct Description {
    pub name: String,
    pub capabilities: ThemeCapabilities,
    pub defaults: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct Create {
    pub mode: UiMode,
    pub settings: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Render(CandidateView),
    Hide,
    Refresh,
    Health,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Reply {
    pub error: Option<String>,
    pub notices: Vec<ThemeNotice>,
    pub events: Vec<(u64, UiAction)>,
}

type Events = Arc<Mutex<VecDeque<(u64, UiAction)>>>;
struct Instance {
    backend: Box<dyn ThemeBackend>,
    events: Events,
    faulted: bool,
}

fn encode<T: Serialize>(value: &T) -> Buffer {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return Buffer {
            data: std::ptr::null_mut(),
            len: 0,
        };
    };
    if bytes.len() > MAX_BYTES {
        return Buffer {
            data: std::ptr::null_mut(),
            len: 0,
        };
    }
    let bytes = bytes.into_boxed_slice();
    let len = bytes.len();
    Buffer {
        data: Box::into_raw(bytes).cast(),
        len,
    }
}

unsafe fn decode<T: DeserializeOwned>(data: *const u8, len: usize) -> Result<T, String> {
    if data.is_null() || len == 0 || len > MAX_BYTES {
        return Err("invalid theme payload length".into());
    }
    serde_json::from_slice(unsafe { std::slice::from_raw_parts(data, len) })
        .map_err(|e| e.to_string())
}

pub fn describe(factory: &dyn ThemeFactory) -> Buffer {
    let result = catch_unwind(AssertUnwindSafe(|| {
        Ok::<_, String>(Description {
            name: factory.name().into(),
            capabilities: factory.capabilities(),
            defaults: factory.default_settings()?,
        })
    }))
    .unwrap_or_else(|_| Err("theme metadata panicked".into()));
    encode(&result)
}

/// Caller supplies valid input bytes and a writable output pointer.
pub unsafe fn create(
    factory: &dyn ThemeFactory,
    data: *const u8,
    len: usize,
    output: *mut *mut c_void,
) -> Buffer {
    if output.is_null() {
        return encode(&Reply {
            error: Some("missing theme output handle".into()),
            ..Default::default()
        });
    }
    unsafe {
        *output = std::ptr::null_mut();
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let request: Create = unsafe { decode(data, len) }?;
        let creation = factory.create(
            request.mode,
            &weasel_common::settings::ConfigSnapshot::new(request.settings),
        );
        let mut reply = Reply {
            notices: creation.notices,
            ..Default::default()
        };
        match creation.backend {
            Ok(backend) => {
                let instance = Box::new(Instance {
                    backend,
                    events: Arc::new(Mutex::new(VecDeque::new())),
                    faulted: false,
                });
                unsafe {
                    *output = Box::into_raw(instance).cast();
                }
            }
            Err(error) => reply.error = Some(error),
        }
        Ok::<_, String>(reply)
    }))
    .unwrap_or_else(|_| Err("theme creation panicked".into()));
    encode(&result.unwrap_or_else(|error| Reply {
        error: Some(error),
        ..Default::default()
    }))
}

/// Called serially on the creating UI thread, with a live instance handle.
pub unsafe fn invoke(handle: *mut c_void, data: *const u8, len: usize) -> Buffer {
    if handle.is_null() {
        return encode(&Reply {
            error: Some("missing theme instance".into()),
            ..Default::default()
        });
    }
    let instance = unsafe { &mut *handle.cast::<Instance>() };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if instance.faulted {
            return Err("theme instance faulted".into());
        }
        let operation: Operation = unsafe { decode(data, len) }?;
        let mut reply = Reply::default();
        let result = match operation {
            Operation::Render(view) => {
                let queue = instance.events.clone();
                let content_id = view.content_id;
                let events = EventSink::new(move |action| {
                    if let Ok(mut queue) = queue.lock() {
                        if queue.len() < 32 {
                            queue.push_back((content_id, action));
                        }
                    }
                });
                instance.backend.render(&view, &events)
            }
            Operation::Hide => {
                instance.backend.hide();
                if let Ok(mut queue) = instance.events.lock() {
                    queue.clear();
                }
                Ok(())
            }
            Operation::Refresh => instance.backend.refresh_appearance(),
            Operation::Health => instance.backend.check_health(),
        };
        reply.error = result.err();
        reply.notices = instance.backend.take_notices();
        if let Ok(mut queue) = instance.events.lock() {
            reply.events.extend(queue.drain(..));
        }
        Ok::<_, String>(reply)
    }));
    let reply = match result {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => Reply {
            error: Some(error),
            ..Default::default()
        },
        Err(_) => {
            instance.faulted = true;
            Reply {
                error: Some("theme operation panicked".into()),
                ..Default::default()
            }
        }
    };
    encode(&reply)
}

/// Destroy on the creating UI thread, before the renderer's apartment closes.
pub unsafe extern "C" fn destroy(handle: *mut c_void) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(handle.cast::<Instance>()))
        }));
    }
}

/// Accepts only an unchanged buffer returned by this DLL.
pub unsafe extern "C" fn release(buffer: Buffer) {
    if !buffer.data.is_null() {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                buffer.data,
                buffer.len,
            )));
        }
    }
}

/// Each DLL exports one entry, backed by its own factory and allocator.
/// No Rust trait object, String, Vec, or callback ownership crosses the ABI.
#[macro_export]
macro_rules! export_theme {
    ($factory:expr) => {
        unsafe extern "C" fn describe() -> $crate::plugin::Buffer {
            $crate::plugin::describe(&$factory)
        }
        unsafe extern "C" fn create(
            data: *const u8,
            len: usize,
            out: *mut *mut std::ffi::c_void,
        ) -> $crate::plugin::Buffer {
            unsafe { $crate::plugin::create(&$factory, data, len, out) }
        }
        unsafe extern "C" fn invoke(
            handle: *mut std::ffi::c_void,
            data: *const u8,
            len: usize,
        ) -> $crate::plugin::Buffer {
            unsafe { $crate::plugin::invoke(handle, data, len) }
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn weasel_theme_entry() -> *const $crate::plugin::PluginApi {
            static API: $crate::plugin::PluginApi = $crate::plugin::PluginApi {
                size: std::mem::size_of::<$crate::plugin::PluginApi>(),
                describe,
                create,
                invoke,
                destroy: $crate::plugin::destroy,
                release: $crate::plugin::release,
            };
            &API
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static DROPPED: AtomicBool = AtomicBool::new(false);
    struct Factory;
    struct Backend;
    impl ThemeFactory for Factory {
        fn name(&self) -> &'static str {
            "contract-test"
        }
        fn capabilities(&self) -> ThemeCapabilities {
            ThemeCapabilities::CANDIDATES_ONLY
        }
        fn create(&self, _: UiMode, _: &weasel_common::settings::ConfigSnapshot) -> ThemeCreation {
            ThemeCreation {
                backend: Ok(Box::new(Backend)),
                notices: vec![ThemeNotice {
                    severity: NoticeSeverity::Warning,
                    code: "config".into(),
                    message: "fallback".into(),
                    details: String::new(),
                }],
            }
        }
    }
    impl ThemeBackend for Backend {
        fn render(&mut self, _: &CandidateView, events: &EventSink) -> Result<(), String> {
            events.send(UiAction::ItemInvoked(2));
            Ok(())
        }
        fn hide(&mut self) {}
        fn refresh_appearance(&mut self) -> Result<(), String> {
            panic!("contained theme panic")
        }
    }
    impl Drop for Backend {
        fn drop(&mut self) {
            DROPPED.store(true, Ordering::Relaxed);
        }
    }
    unsafe fn read_reply(buffer: Buffer) -> Reply {
        let reply = unsafe { decode(buffer.data, buffer.len) }.unwrap();
        unsafe {
            release(buffer);
        }
        reply
    }

    #[test]
    fn bridge_preserves_notices_event_identity_and_destroys_faulted_instances() {
        let bytes = serde_json::to_vec(&Create {
            mode: UiMode::Live,
            settings: serde_json::json!({}),
        })
        .unwrap();
        let mut handle = std::ptr::null_mut();
        unsafe {
            let reply = read_reply(create(&Factory, bytes.as_ptr(), bytes.len(), &mut handle));
            assert!(reply.error.is_none());
            assert_eq!(reply.notices[0].code, "config");
            assert!(!handle.is_null());
            let render = serde_json::to_vec(&Operation::Render(CandidateView {
                content_id: 42,
                ..Default::default()
            }))
            .unwrap();
            let reply = read_reply(invoke(handle, render.as_ptr(), render.len()));
            assert_eq!(reply.events, [(42, UiAction::ItemInvoked(2))]);
            let refresh = serde_json::to_vec(&Operation::Refresh).unwrap();
            assert!(
                read_reply(invoke(handle, refresh.as_ptr(), refresh.len()))
                    .error
                    .unwrap()
                    .contains("panicked")
            );
            assert!(
                read_reply(invoke(handle, render.as_ptr(), render.len()))
                    .error
                    .unwrap()
                    .contains("faulted")
            );
            destroy(handle);
            assert!(DROPPED.load(Ordering::Relaxed));
        }
    }
}
