//! 管理 TSF 单元中的文本服务状态，并将 COM 回调分派给职责独立的子模块。
//!
//! TSF 接口调用必须遵守宿主编辑会话和重入规则；文本修改通过排队的编辑会话执行，
//! 异步响应则使用上下文令牌校验，避免过期结果影响当前文档。
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
mod callbacks;
mod composition;
mod context;
mod host_edit;
use context::ContextState;
mod display_attribute;
mod edit_session;
mod input_mode;
mod key_event;
mod language_bar;
mod layout;
mod lifecycle;
mod range;
mod response;
mod safety;
mod secure_input;
use display_attribute::{DisplayAttributeEnumerator, DisplayAttributeInfo};

use std::{
    collections::VecDeque,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

use crate::bindings::{
    CLSCTX_INPROC_SERVER, CLSID_TF_CategoryMgr, E_NOINTERFACE, E_NOTIMPL, E_POINTER,
    GUID_PROP_ATTRIBUTE, GUID_WEASEL_DISPLAY_ATTRIBUTE, IEnumTfDisplayAttributeInfo,
    IEnumTfDisplayAttributeInfo_Impl, ITfActiveLanguageProfileNotifySink,
    ITfActiveLanguageProfileNotifySink_Impl, ITfCategoryMgr, ITfCompartmentEventSink,
    ITfCompartmentEventSink_Impl, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextComposition, ITfContextView, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl,
    ITfDocumentMgr, ITfEditRecord, ITfEditSession, ITfEditSession_Impl, ITfInputScope,
    ITfInsertAtSelection, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfProperty,
    ITfRange, ITfReadOnlyProperty, ITfSource, ITfTextEditSink, ITfTextEditSink_Impl,
    ITfTextInputProcessor, ITfTextInputProcessor_Impl, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextLayoutSink, ITfTextLayoutSink_Impl, ITfThreadFocusSink,
    ITfThreadFocusSink_Impl, ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    LPARAM, RECT, S_FALSE, TF_AE_NONE, TF_ANCHOR_END, TF_ANCHOR_START, TF_ATTR_INPUT, TF_CT_NONE,
    TF_DA_COLOR, TF_DA_COLOR_0, TF_DISPLAYATTRIBUTE, TF_ES_ASYNCDONTCARE, TF_ES_READ,
    TF_ES_READWRITE, TF_IAS_QUERYONLY, TF_LS_DOT, TF_SELECTION, TF_SELECTIONSTYLE,
    TF_TMF_SECUREMODE, TfClientId, TfEditCookie, TfGuidAtom, TfLayoutCode, VARIANT, VARIANT_0,
    VARIANT_0_0, VARIANT_0_0_0, VARTYPE, VT_I4, VT_UNKNOWN, WPARAM,
};
use crate::rpc_worker::RpcWorker;
use crate::{bindings, boundary, update_window};
use weasel_common::message::{KeyEventResponse, LayoutUpdate, RenderRect};
use windows_core::{BOOL, Error, GUID, IUnknown, IUnknownImpl, Interface, Ref, Result, implement};

use crate::module::ModuleLease;

#[derive(Clone, Copy)]
/// 一项待执行的宿主文本编辑操作；各步骤由编辑会话调度器推进。
enum EditStep {
    /// 校验并应用引擎响应。
    ApplyResponse,
    /// 为当前组合创建 TSF composition。
    StartComposition,
    /// 更新现有组合文本及光标。
    UpdateComposition,
    /// 提交组合并结束 composition。
    CommitComposition,
    /// 在选区处插入已提交文本。
    InsertCommit,
    /// 清理由宿主或连接变化遗留的 composition。
    DisconnectComposition,
    /// 结束原始按键输入期间暂存的 composition。
    FinishRawComposition,
    /// 结束 composition，并按标志清理其内容或排队后续组合。
    EndComposition {
        /// 是否同时清除组合文本、显示属性及保存的宿主选区。
        clear: bool,
        /// 是否根据响应的 composing 状态请求后续组合。
        restart: bool,
    },
}

impl EditStep {
    /// 返回用于诊断日志的稳定步骤名称。
    fn name(self) -> &'static str {
        match self {
            Self::ApplyResponse => "ApplyResponse",
            Self::StartComposition => "StartComposition",
            Self::UpdateComposition => "UpdateComposition",
            Self::CommitComposition => "CommitComposition",
            Self::InsertCommit => "InsertCommit",
            Self::DisconnectComposition => "DisconnectComposition",
            Self::FinishRawComposition => "FinishRawComposition",
            Self::EndComposition { .. } => "EndComposition",
        }
    }
}

/// 等待 TSF 编辑会话执行的一项工作及其宿主对象引用。
struct PendingEdit {
    /// 工作所属上下文；其代次用于拒绝过期响应。
    state: Arc<ContextState>,
    /// 接受编辑会话请求的 TSF 文档上下文。
    context: ITfContext,
    /// 与此工作关联的引擎响应。
    response: KeyEventResponse,
    /// 本次会话应执行的操作。
    step: EditStep,
    /// 发起编辑会话时的 COM 对象，供 TSF 回调周期内继续使用。
    session: IUnknown,
}

/// 为不支持的 COM 操作返回标准 `E_NOTIMPL`。
fn not_implemented<T>() -> Result<T> {
    Err(Error::from_hresult(E_NOTIMPL))
}

impl PendingEdit {
    /// 检查工作是否仍属于有效上下文代次；断连清理允许不同的令牌校验规则。
    fn matches(&self) -> Result<bool> {
        if matches!(self.step, EditStep::DisconnectComposition) {
            return Ok(self.state.alive.load(Ordering::Acquire)
                && !self.state.suspended.load(Ordering::Acquire)
                && self.response.token.as_ref().is_some_and(|t| {
                    t.generation == self.state.generation.load(Ordering::Acquire)
                }));
        }
        self.state.matches(self.response.token.as_ref())
    }
}

#[implement(
    ITfTextInputProcessor,
    ITfTextInputProcessorEx,
    ITfThreadMgrEventSink,
    ITfTextEditSink,
    ITfTextLayoutSink,
    ITfKeyEventSink,
    ITfThreadFocusSink,
    ITfCompositionSink,
    ITfActiveLanguageProfileNotifySink,
    ITfDisplayAttributeProvider,
    ITfCompartmentEventSink
)]
/// 暴露 TSF 文本服务接口并持有线程单元内的上下文、调度器和连接状态。
pub(crate) struct TextService {
    /// 当前存活的文档上下文状态。
    contexts: Mutex<Vec<Arc<ContextState>>>,
    /// 当前焦点上下文 ID；无焦点时为空。
    focused_context: Mutex<Option<u64>>,
    /// 配对 TSF 测试按键与实际按键回调的单项缓存。
    tested_key: Mutex<Option<key_event::TestedKey>>,
    /// 服务是否已激活并可接收输入。
    activated: AtomicBool,
    /// 释放资源因重入而延后时置位，供后续维护重试。
    teardown_pending: AtomicBool,
    /// 阻止并发或重入执行重复的拆卸流程。
    tearing_down: AtomicBool,
    /// 当前 TSF 编辑会话是否已经修改宿主文本。
    edit_mutated: AtomicBool,
    /// 记录故障、隔离上下文并请求维护的状态。
    faulted: crate::diagnostics::FaultState,
    /// 服务级激活代次，用于使旧编辑预约失效。
    generation: Arc<AtomicU64>,
    /// 与进程外输入引擎通信的工作线程。
    rpc: Arc<Mutex<RpcWorker>>,
    /// 用于在 TSF 回调之外请求维护的窗口。
    update_window: Mutex<Option<update_window::UpdateWindow>>,
    /// TSF 语言栏对象。
    language_bar: Mutex<Option<Arc<language_bar::LanguageBar>>>,
    /// 当前宿主线程管理器 COM 接口。
    thread_mgr: Mutex<Option<ITfThreadMgr>>,
    /// 线程管理器事件接收器的订阅 cookie。
    thread_mgr_event_sink_cookie: Mutex<Option<u32>>,
    /// 线程焦点事件接收器的订阅 cookie。
    thread_focus_sink_cookie: Mutex<Option<u32>>,
    /// TSF 按键管理器接口。
    keystroke_mgr: Mutex<Option<ITfKeystrokeMgr>>,
    /// 注册 TSF 按键接收器时分配的客户端 ID。
    keystroke_client_id: Mutex<Option<TfClientId>>,
    /// 按序等待宿主读写编辑会话执行的任务。
    pending_edit: Mutex<VecDeque<PendingEdit>>,
    /// 编辑会话预约是否已占用。
    edit_requested: AtomicBool,
    /// 编辑预约票号，用于防止旧预约清除新请求。
    edit_ticket: AtomicU64,
    /// 当前活动编辑上下文 ID。
    active_edit_context: AtomicU64,
    /// 注册后供 TSF 文本属性使用的显示属性原子值。
    display_attribute_atom: Mutex<Option<TfGuidAtom>>,
    /// 宿主 TSF 是否处于安全模式。
    secure_mode: AtomicBool,
    /// 配置是否允许在安全输入字段中调用引擎。
    allow_rime_in_secure_fields: AtomicBool,
    /// 安全输入策略代次；策略变化时使先前探测结果失效。
    secure_policy_epoch: AtomicU64,
    /// 保证此 COM 服务实例存活期间实现模块不会卸载。
    _module: ModuleLease,
}

impl TextService {
    #[track_caller]
    /// 以非阻塞方式获取 TSF 单元状态锁。
    ///
    /// COM 回调可能在调用 TSF 或宿主代码时重入，因此锁竞争返回 `E_PENDING` 并请求维护；
    /// 锁中毒则记录故障并返回 `E_FAIL`，绝不在回调线程等待锁释放。
    fn lock<'a, T>(&'a self, state: &'a Mutex<T>) -> Result<std::sync::MutexGuard<'a, T>> {
        match state.try_lock() {
            Ok(guard) => Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.faulted.request_maintenance();
                Err(Error::from_hresult(boundary::E_PENDING))
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                let address = state as *const Mutex<T> as usize as u64;
                self.faulted.mark("lock.poisoned", address);
                Err(Error::from_hresult(boundary::E_FAIL))
            }
        }
    }

    /// 初始化尚未激活的服务状态和所有 TSF/RPC 资源槽位。
    pub(crate) fn new() -> Self {
        Self {
            contexts: Mutex::new(Vec::new()),
            focused_context: Mutex::new(None),
            tested_key: Mutex::new(None),
            activated: AtomicBool::new(false),
            teardown_pending: AtomicBool::new(false),
            tearing_down: AtomicBool::new(false),
            edit_mutated: AtomicBool::new(false),
            faulted: crate::diagnostics::FaultState::new(false),
            generation: Arc::new(AtomicU64::new(0)),
            rpc: Arc::new(Mutex::new(RpcWorker::default())),
            update_window: Mutex::new(None),
            language_bar: Mutex::new(None),
            thread_mgr: Mutex::new(None),
            thread_mgr_event_sink_cookie: Mutex::new(None),
            thread_focus_sink_cookie: Mutex::new(None),
            keystroke_mgr: Mutex::new(None),
            keystroke_client_id: Mutex::new(None),
            pending_edit: Mutex::new(VecDeque::new()),
            edit_requested: AtomicBool::new(false),
            edit_ticket: AtomicU64::new(0),
            active_edit_context: AtomicU64::new(0),
            display_attribute_atom: Mutex::new(None),
            secure_mode: AtomicBool::new(false),
            allow_rime_in_secure_fields: AtomicBool::new(false),
            secure_policy_epoch: AtomicU64::new(0),
            _module: ModuleLease::new(),
        }
    }
}

impl Drop for TextService {
    /// 最后一个 COM 持有者释放服务时执行可重入安全的内部拆卸。
    fn drop(&mut self) {
        self.deactivate_internal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn poisoned_state_is_contained_at_com_boundary_and_drop_is_safe() {
        let service = windows_core::ComObject::new(TextService::new());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _lock = service.tested_key.lock().unwrap();
            panic!("injected poisoned input cache");
        }));
        let keys: ITfKeyEventSink = service.to_interface();
        assert!(unsafe { keys.OnSetFocus(true) }.is_err());
        assert!(service.faulted.load(Ordering::Acquire));
        assert!(
            !unsafe { keys.OnTestKeyDown(None, WPARAM(65), LPARAM(0)) }
                .unwrap()
                .as_bool()
        );
        let processor: ITfTextInputProcessor = service.to_interface();
        unsafe {
            processor.Deactivate().unwrap();
            processor.Deactivate().unwrap();
        }
        // Dropping all COM references also exercises no-panic teardown.
    }

    #[test]
    fn reentrant_state_access_fails_without_waiting() {
        let service = TextService::new();
        let guard = service.tested_key.lock().unwrap();
        assert_eq!(
            service.lock(&service.tested_key).err().unwrap().code(),
            boundary::E_PENDING
        );
        assert!(!service.faulted.load(Ordering::Acquire));
        drop(guard);
        assert!(service.lock(&service.tested_key).is_ok());
    }

    #[test]
    fn teardown_busy_state_is_retained_until_retry() {
        let service = TextService::new();
        service.activated.store(true, Ordering::Release);
        let guard = service.tested_key.lock().unwrap();
        service.deactivate_internal();
        assert!(service.teardown_pending.load(Ordering::Acquire));
        assert!(!service.activated.load(Ordering::Acquire));
        assert!(!service.faulted.load(Ordering::Acquire));
        drop(guard);
        service.deactivate_internal();
        assert!(!service.teardown_pending.load(Ordering::Acquire));
    }
}
