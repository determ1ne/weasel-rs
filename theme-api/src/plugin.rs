//! 同构建版本主题 DLL 的 C ABI 桥接层。
//!
//! ABI 入口仅交换定长结构、句柄和 JSON 字节缓冲区；Rust trait 对象、字符串、向量
//! 与回调所有权均留在所属 DLL 的分配器内。宿主必须用返回缓冲区所属 DLL 的
//! `release` 释放它，并在创建句柄的 UI 线程串行调用及销毁实例。入口会捕获跨越
//! 桥接调用的 Rust panic，但这不隔离原生故障、死锁，也不能代替主题对原生窗口
//! 回调中 panic 的处理。协议只保证同构建版本之间兼容，不能视为稳定的跨版本 ABI。
use crate::*;
use serde::de::DeserializeOwned;
use std::{
    collections::VecDeque,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

/// 单个桥接 JSON 消息允许的最大字节数。
///
/// 编码超限时返回空缓冲区；解码超限时返回输入长度错误。
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// DLL 导出入口的 NUL 结尾名称。
///
/// 宿主通过此符号取得 [`PluginApi`]；名称按字节提供，末尾 NUL 是符号名的一部分。
pub const ENTRY: &[u8] = b"weasel_theme_entry\0";

/// 跨 DLL 返回的拥有型字节缓冲区。
///
/// 缓冲区由创建它的 DLL 持有，消费者只能读取 `len` 个字节，并必须将原样的
/// 指针和长度交回该 DLL 的 [`release`] 释放。不得修改、复制后释放、重复释放，
/// 也不得在释放后继续使用。空缓冲区由空指针与零长度表示。
#[repr(C)]
pub struct Buffer {
    /// 缓冲区首地址；非空时指向由生产方分配的字节数组。
    pub data: *mut u8,
    /// 可读取的字节数。
    pub len: usize,
}

/// 主题 DLL 的版本内函数表。
///
/// `#[repr(C)]` 固定字段顺序和 C 布局。宿主应先确认 `size` 足以覆盖其将访问的
/// 字段，再调用函数指针；函数返回的 [`Buffer`] 仍须由同一 DLL 的 `release` 释放。
/// 该表和函数指针由导出 DLL 静态持有，宿主不得修改或释放。
#[repr(C)]
pub struct PluginApi {
    /// 当前函数表结构的字节大小，用于检查双方布局是否兼容。
    pub size: usize,
    /// 获取主题名称、能力和默认设置的 JSON 回复。
    pub describe: unsafe extern "C" fn() -> Buffer,
    /// 创建主题实例；输入为 [`Create`] 的 JSON，输出句柄写入调用方指针。
    pub create: unsafe extern "C" fn(*const u8, usize, *mut *mut c_void) -> Buffer,
    /// 对已有实例执行 [`Operation`] 指定的操作。
    pub invoke: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> Buffer,
    /// 在创建线程销毁实例句柄。
    pub destroy: unsafe extern "C" fn(*mut c_void),
    /// 释放由本 DLL 生产且未更改的字节缓冲区。
    pub release: unsafe extern "C" fn(Buffer),
}

/// `describe` 操作的 JSON 结果。
#[derive(Serialize, Deserialize)]
pub struct Description {
    /// 工厂提供的主题名称。
    pub name: String,
    /// 主题声明的展示能力。
    pub capabilities: ThemeCapabilities,
    /// 主题本地默认配置；宿主可在创建前叠加安装级和用户配置。
    pub defaults: serde_json::Value,
}

/// 创建主题实例时发送的 JSON 请求。
#[derive(Serialize, Deserialize)]
pub struct Create {
    /// 实例运行场景。
    pub mode: UiMode,
    /// 宿主合并后的主题配置。具体键和语义由主题定义。
    pub settings: serde_json::Value,
}

/// 对现有主题实例执行的操作。
#[derive(Serialize, Deserialize)]
pub enum Operation {
    /// 使用完整快照渲染 UI。
    Render(CandidateView),
    /// 隐藏 UI 并清空尚未交付的事件。
    Hide,
    /// 使外观资源失效并按需重建。
    Refresh,
    /// 检查实例健康状态。
    Health,
}

/// 桥接操作返回的 JSON 回复。
///
/// 发生可报告错误时由 `error` 携带错误文本；操作产生的通知和已排队事件也通过
/// 同一回复交付。未设置错误字段表示桥接层未报告错误，不代表宿主已处理事件。
#[derive(Default, Serialize, Deserialize)]
pub struct Reply {
    /// 操作错误或 panic 的说明；正常完成时为 `None`。
    pub error: Option<String>,
    /// 本次操作后从后端收集的通知。
    pub notices: Vec<ThemeNotice>,
    /// 已发生的用户操作及其展示内容标识。
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

/// 查询工厂元数据并编码为 JSON 缓冲区。
///
/// 工厂 panic 会转换为包含错误文本的结果；JSON 编码失败或超过 [`MAX_BYTES`]
/// 时返回空缓冲区。非空返回值归调用方所有，必须交给同一 DLL 的 [`release`]。
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

/// 创建后端实例并返回创建回复。
///
/// # 安全性
///
/// `data` 必须指向至少 `len` 个可读字节，且在调用期间有效；`output` 必须指向
/// 可写的 `*mut c_void`。输出指针不能与输入区域或其他无效内存重叠。成功时
/// `*output` 获得不透明实例句柄，之后必须在创建它的 UI 线程串行传给 [`invoke`]，
/// 并恰好一次传给 [`destroy`]；失败时输出被置为空指针。返回缓冲区由本 DLL 分配，
/// 调用方必须用本 DLL 的 [`release`] 释放。
///
/// 输入无效、工厂创建失败或发生 Rust panic 时，错误写入回复；缓冲区编码失败或
/// 超限时返回空缓冲区。创建产生的通知即使后端创建失败仍会放入回复。
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

/// 在实例所属 UI 线程对其串行执行一次操作。
///
/// # 安全性
///
/// `handle` 必须是本 DLL 的 [`create`] 成功返回、尚未销毁且当前未被并发使用的
/// 实例句柄。`data` 必须指向至少 `len` 个可读字节，且在调用期间有效。调用方须
/// 保证句柄只在创建线程使用，并最终传给 [`destroy`] 一次。违反这些条件可能导致
/// 未定义行为。
///
/// 无效输入和后端错误通过回复的 `error` 字段报告。操作 panic 会将实例标记为
/// 故障；此后调用返回故障错误，直到调用方销毁实例。事件在回复中带有产生它们的
/// 内容标识。返回缓冲区须由本 DLL 的 [`release`] 释放；编码失败或超限时返回空缓冲区。
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

/// 销毁实例并释放其后端资源。
///
/// 必须在创建实例的 UI 线程调用，且应早于渲染器 apartment 关闭。空句柄是无操作；
/// 非空句柄必须是本 DLL 创建且尚未销毁的实例，并且不得再用于其他入口。函数会
/// 捕获析构期间的 Rust panic。此函数不返回错误，也不释放其他桥接缓冲区。
pub unsafe extern "C" fn destroy(handle: *mut c_void) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(handle.cast::<Instance>()))
        }));
    }
}

/// 释放本 DLL 之前返回且未被更改的缓冲区。
///
/// # 安全性
///
/// 非空 `buffer.data` 必须来自同一 DLL 的桥接入口，`buffer.len` 必须与返回时一致，
/// 且该缓冲区尚未释放。不得传入伪造、改写或重复释放的缓冲区。空指针无需释放。
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

/// 为当前 DLL 导出主题 ABI 入口。
///
/// 宏参数是该 DLL 内部的主题工厂表达式；宏生成固定名称
/// `weasel_theme_entry`，供宿主取得 [`PluginApi`]。每个 DLL 应只导出一个此入口。
/// 工厂、Rust trait 对象、字符串、向量和回调均由本 DLL 的分配器持有，不跨 ABI
/// 转移所有权；ABI 仅传递 C 布局函数表、实例不透明句柄及 JSON 缓冲区。宿主与主题
/// 必须使用同一构建版本的契约。入口捕获桥接函数中的 Rust panic，但不防御原生回调
/// panic、原生崩溃或死锁。
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
