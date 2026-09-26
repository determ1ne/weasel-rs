//! 加载主题 DLL

use crate::d2d_bindings::*;
use crate::theme_api::*;
use std::{ffi::c_void, path::Path};
use weasel_theme_api::plugin::{self, Buffer, Create, Description, Operation, PluginApi, Reply};
use windows_strings::{PCSTR, PCWSTR};

/// 已验证的主题插件入口、能力描述和默认设置。
///
/// `api` 指向 DLL 中的静态表，因此工厂要求对应模块在进程剩余时间内保持加载。
pub struct Factory {
    /// 与注册表对应的稳定主题名称。
    name: &'static str,
    /// 插件 DLL 导出的静态 ABI 函数表。
    api: &'static PluginApi,
    /// 加载时验证过的能力和主题默认设置。
    description: Description,
}

/// 确保插件拥有的响应缓冲区恰好交还给同一插件 API。
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

/// 校验插件缓冲区边界并反序列化 JSON；所有返回路径都会释放输入缓冲区。
fn read<T: serde::de::DeserializeOwned>(api: &PluginApi, buffer: Buffer) -> Result<T, String> {
    let buffer = BufferGuard(api, buffer);
    if buffer.1.data.is_null() || buffer.1.len == 0 || buffer.1.len > plugin::MAX_BYTES {
        return Err("theme returned an invalid payload".into());
    }
    serde_json::from_slice(unsafe { std::slice::from_raw_parts(buffer.1.data, buffer.1.len) })
        .map_err(|e| format!("invalid theme response: {e}"))
}

impl Factory {
    /// 从指定路径加载并验证主题 DLL 的 ABI 与元数据。
    ///
    /// 仅允许从 DLL 所在目录和系统目录解析依赖。验证失败会释放模块；加载成功
    /// 后模块故意不卸载，以保证仍存活的原生回调地址有效。
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
    /// 返回注册名、能力、默认设置，并在插件侧创建主题实例。
    ///
    /// 设置按 JSON 编码且受插件 ABI 的最大载荷限制。创建过程失败时返回错误，
    /// 不会向调用方暴露半初始化后端；后端创建和后续调用应由 UI apartment 执行。
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
    /// 指向进程驻留 DLL 中的函数表。
    api: &'static PluginApi,
    /// 插件实例句柄；非空时由 `destroy` 恰好销毁一次。
    handle: *mut c_void,
    /// 当前内容代号及其事件接收端，用于过滤插件返回的过期动作。
    events: Option<(u64, EventSink)>,
    /// 等待渲染器统一排出的插件通知。
    notices: Vec<ThemeNotice>,
    /// 隐藏阶段遇到错误后缓存的故障，阻止后续插件调用。
    failure: Option<String>,
}

impl Backend {
    /// 编码并执行一次有界插件操作，收集通知并转发当前内容的事件。
    ///
    /// 插件调用是同步的，可能在 UI 线程上执行；操作载荷超限、ABI 响应无效、
    /// 插件报告错误或后端已进入故障状态时返回错误。事件只转发与当前内容代号
    /// 相同的结果，避免迟到响应触发旧界面动作。
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
    /// 取出当前累积的通知，避免重复报告。
    fn take_notices(&mut self) -> Vec<ThemeNotice> {
        std::mem::take(&mut self.notices)
    }
    /// 设置事件接收端后提交当前视图。
    fn render(&mut self, view: &CandidateView, events: &EventSink) -> Result<(), String> {
        self.events = Some((view.content_id, events.clone()));
        self.call(Operation::Render(view.clone()))
    }
    /// 清除事件接收端并请求插件隐藏；失败会被记为后端故障。
    fn hide(&mut self) {
        self.events = None;
        if let Err(error) = self.call(Operation::Hide) {
            self.failure = Some(error);
        }
    }
    /// 通知插件系统外观可能已改变。
    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.call(Operation::Refresh)
    }
    /// 查询插件健康状态；错误交由运行时结束当前后端。
    fn check_health(&mut self) -> Result<(), String> {
        self.call(Operation::Health)
    }
}

impl Drop for Backend {
    /// 通过插件 ABI 销毁实例；调用方必须在创建实例的 UI apartment 上析构。
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.api.destroy)(self.handle);
            }
        }
    }
}
