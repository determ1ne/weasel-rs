//! 在 TSF 读编辑会话中检查当前选区的输入范围，并将密码字段状态保存在文档上下文中。
//!
//! 状态尚未探测或探测结果失效时按安全状态处理；探测只在有效的编辑 cookie
//! 内访问文本存储，异步结果则通过票据和上下文代次避免覆盖更新状态。
use super::*;
use crate::bindings::{
    CoTaskMemFree, GUID_PROP_INPUTSCOPE, IS_NUMERIC_PASSWORD, IS_PASSWORD, InputScopeManual,
    TF_DEFAULT_SELECTION, TF_ES_SYNC, VariantClear,
};
use weasel_common::message::ContextAction;

pub(super) const UNKNOWN: u8 = 0;
/// 已探测为普通文本字段。
const NORMAL: u8 = 1;
/// 已探测为密码或数字密码字段。
const SECURE: u8 = 2;
/// 输入范围数组超过此上限时视为无效的宿主返回值。
const MAX_INPUT_SCOPES: u32 = 64;

#[derive(Default)]
/// 合并同一上下文的密码字段探测，并使已被替代的回调失效。
pub(super) struct SecureProbeSchedule {
    /// 单调递增的本地序号；回绕不影响当前待处理票据的判等。
    serial: u64,
    /// 唯一有效的待处理探测票据。
    pending: Option<u64>,
}

impl SecureProbeSchedule {
    /// 请求探测；`replace` 为真时以新票据取代旧请求。
    fn request(&mut self, replace: bool) -> Option<u64> {
        if self.pending.is_some() && !replace {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some(self.serial);
        Some(self.serial)
    }

    /// 仅消费当前票据，防止迟到的旧会话清除新请求。
    fn take(&mut self, ticket: u64) -> bool {
        if self.pending == Some(ticket) {
            self.pending = None;
            true
        } else {
            false
        }
    }
}

struct ScopeBuffer(*mut InputScopeManual);

/// 释放 `GetInputScopes` 通过 COM 分配器返回的数组。
impl Drop for ScopeBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }
}

/// 在调用方提供的 TSF 编辑 cookie 下取得默认选区，并接管返回的 COM 引用。
fn selected_range(context: &ITfContext, ec: TfEditCookie) -> Result<Option<ITfRange>> {
    let mut selection = TF_SELECTION {
        range: std::mem::ManuallyDrop::new(None),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: BOOL(0),
        },
    };
    let mut fetched = 0;
    let result =
        unsafe { context.GetSelection(ec, TF_DEFAULT_SELECTION, 1, &mut selection, &mut fetched) };
    let range = unsafe { std::mem::ManuallyDrop::take(&mut selection.range) };
    result.ok()?;
    Ok((fetched == 1).then_some(range).flatten())
}

/// 从 `VARIANT` 中克隆 `IUnknown` 引用；由调用方清理原 `VARIANT`。
fn input_scope_unknown(value: &mut VARIANT) -> Option<IUnknown> {
    unsafe {
        let inner = &*value.Anonymous.Anonymous;
        if inner.vt != VARTYPE(VT_UNKNOWN as u16) {
            return None;
        }
        // Keep our own COM reference and let VariantClear release the one
        // owned by the VARIANT; never mutate an active union member by hand.
        (&*inner.Anonymous.punkVal).clone()
    }
}

/// 读取当前选区的应用输入范围属性。缺少属性或不支持该接口视为普通文本；
/// TSF 调用或返回数组越界等结构异常则作为错误交给编辑会话边界处理。
fn query_secure_field(context: &ITfContext, ec: TfEditCookie) -> Result<bool> {
    let Some(range) = selected_range(context, ec)? else {
        return Ok(false);
    };
    // Missing app properties are normal for ordinary Win32 text stores.
    let Ok(property): Result<ITfReadOnlyProperty> =
        (unsafe { context.GetAppProperty(&GUID_PROP_INPUTSCOPE) })
    else {
        return Ok(false);
    };
    let Ok(mut value) = (unsafe { property.GetValue(ec, &range) }) else {
        return Ok(false);
    };
    let unknown = input_scope_unknown(&mut value);
    unsafe {
        let _ = VariantClear(&mut value);
    }
    let Some(unknown) = unknown else {
        return Ok(false);
    };
    let Ok(input_scope) = unknown.cast::<ITfInputScope>() else {
        return Ok(false);
    };
    let mut scopes = std::ptr::null_mut();
    let mut count = 0;
    unsafe { input_scope.GetInputScopes(&mut scopes, &mut count).ok()? };
    let scopes = ScopeBuffer(scopes);
    if count > MAX_INPUT_SCOPES || (count != 0 && scopes.0.is_null()) {
        return Err(Error::from_hresult(boundary::E_FAIL));
    }
    let values = if count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(scopes.0, count as usize) }
    };
    Ok(values
        .iter()
        .any(|scope| matches!(*scope, IS_PASSWORD | IS_NUMERIC_PASSWORD)))
}

#[implement(ITfEditSession)]
/// 在 TSF 读会话中探测密码字段；回调只对创建时的票据和上下文代次生效。
struct SecureFieldProbe {
    /// 由 `_owner` 保活的文本服务对象地址。
    service: *const TextService,
    /// 探测目标；回调前后均以该上下文状态核验存活和代次。
    state: Arc<ContextState>,
    /// 本探测在调度器中的唯一票据。
    ticket: u64,
    /// 创建会话时记录的上下文代次，防止旧焦点探测写回新状态。
    generation: u64,
    /// 保持服务对象和 COM 模块在 TSF 回调期间存活。
    _owner: IUnknown,
    _module: ModuleLease,
}

impl Drop for SecureFieldProbe {
    /// 若 TSF 丢弃会话而未执行，撤销仍属于本探测的待处理票据。
    fn drop(&mut self) {
        if let Ok(mut schedule) = self.state.secure_probe.try_lock() {
            schedule.take(self.ticket);
        }
    }
}

impl ITfEditSession_Impl for SecureFieldProbe_Impl {
    /// 消费有效探测请求，在 TSF 提供的读 cookie 下查询输入范围并更新状态。
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            let requested = self
                .state
                .secure_probe
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .take(self.ticket);
            if !requested
                || !self.state.alive.load(Ordering::Acquire)
                || self.state.generation.load(Ordering::Acquire) != self.generation
            {
                return Ok(());
            }
            let secure = query_secure_field(&self.state.context, ec)?;
            let service = unsafe { &*self.service };
            service.set_secure_field(&self.state, secure);
            Ok(())
        })
    }
}

impl TextService {
    /// 安排密码字段读取。键盘边界可用同步读会话替代先前的异步焦点探测，
    /// 避免按键决策依赖尚未执行的旧回调。
    pub(super) fn request_secure_field_probe(
        &self,
        state: &Arc<ContextState>,
        owner: IUnknown,
        synchronous: bool,
    ) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || !state.alive.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        // A key callback must not wait for an older asynchronous focus probe:
        // supersede it with a synchronous read and let the old callback no-op.
        let Some(ticket) = self.lock(&state.secure_probe)?.request(synchronous) else {
            return Ok(());
        };
        let probe: ITfEditSession = SecureFieldProbe {
            service: self,
            state: state.clone(),
            ticket,
            generation: state.generation.load(Ordering::Acquire),
            _owner: owner,
            _module: ModuleLease::new(),
        }
        .into();
        let flags = if synchronous {
            TF_ES_SYNC | TF_ES_READ
        } else {
            TF_ES_ASYNCDONTCARE | TF_ES_READ
        };
        let result = unsafe { state.context.RequestEditSession(tid, &probe, flags) }
            .and_then(|session| session.ok());
        if result.is_err() {
            self.lock(&state.secure_probe)?.take(ticket);
        }
        result
    }

    /// 在已有编辑 cookie 中刷新密码字段状态；探测失败时保留原状态。
    pub(super) fn refresh_secure_field_from_cookie(&self, state: &ContextState, ec: TfEditCookie) {
        if let Ok(secure) = query_secure_field(&state.context, ec) {
            self.set_secure_field(state, secure);
        }
    }

    /// 原子更新字段状态；只有状态实际变化时才通知更新窗口重新协调输入。
    fn set_secure_field(&self, state: &ContextState, secure: bool) {
        let next = if secure { SECURE } else { NORMAL };
        if state.secure_field.swap(next, Ordering::AcqRel) == next {
            return;
        }
        self.wake_for_secure_update();
    }

    /// 将状态变化投递给服务窗口，避免在 TSF 回调中直接重入输入协调流程。
    fn wake_for_secure_update(&self) {
        let hwnd = self
            .update_window
            .try_lock()
            .ok()
            .and_then(|window| window.as_ref().map(|window| window.hwnd));
        if let Some(hwnd) = hwnd {
            unsafe {
                let _ = bindings::PostMessageW(
                    Some(hwnd),
                    update_window::UPDATE_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    /// 返回当前字段是否应按密码字段对外呈现，包含全局安全模式。
    pub(super) fn secure_field_visible(&self, state: &ContextState) -> bool {
        self.secure_mode.load(Ordering::Acquire)
            || state.secure_field.load(Ordering::Acquire) == SECURE
    }

    /// 只有当前 RPC 连接代次明确允许时，才允许在密码字段中使用 Rime。
    pub(super) fn allow_rime_for_secure_field(&self, state: &ContextState) -> Result<bool> {
        let epoch = self.lock(&state.rpc)?.connection_epoch();
        Ok(epoch != 0
            && self.secure_policy_epoch.load(Ordering::Acquire) == epoch
            && self.allow_rime_in_secure_fields.load(Ordering::Acquire))
    }

    /// 判断安全策略是否要求绕过 Rime；尚未知晓的字段状态同样采取保守处理。
    pub(super) fn should_bypass_secure_field(&self, state: &ContextState) -> Result<bool> {
        Ok(!self.allow_rime_for_secure_field(state)?
            && (self.secure_mode.load(Ordering::Acquire)
                || state.secure_field.load(Ordering::Acquire) != NORMAL))
    }

    /// 将焦点上下文与安全策略对齐；首次进入绕过状态时取消远端组合，且只取消一次。
    pub(super) fn reconcile_secure_field(&self) -> Result<()> {
        let focused = *self.lock(&self.focused_context)?;
        let state = self
            .lock(&self.contexts)?
            .iter()
            .find(|state| Some(state.id) == focused)
            .cloned();
        let Some(state) = state else {
            return Ok(());
        };
        // Unknown is fail-closed only at the key boundary. Do not invalidate
        // the context while its asynchronous focus probe is still pending.
        let secure = self.secure_field_visible(&state);
        let bypass = secure && !self.allow_rime_for_secure_field(&state)?;
        if !bypass {
            state.secure_bypass_active.store(false, Ordering::Release);
            return Ok(());
        }
        self.lock(&self.tested_key)?.take();
        if state.secure_bypass_active.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let connected = state.token()?.connection_epoch != 0;
        let has_composition = self.lock(&state.composition)?.is_some();
        if connected || has_composition {
            self.send_context_action(&state, ContextAction::Cancel, true)?;
        }
        Ok(())
    }

    /// 记录连接代次对应的安全字段许可，并在策略变化后唤醒主窗口重新协调。
    pub(super) fn update_secure_policy(&self, allow: Option<bool>, epoch: u64) {
        let allow = allow.unwrap_or(false);
        let changed = self
            .allow_rime_in_secure_fields
            .swap(allow, Ordering::AcqRel)
            != allow;
        let epoch_changed = self.secure_policy_epoch.swap(epoch, Ordering::AcqRel) != epoch;
        if changed || epoch_changed {
            self.wake_for_secure_update();
        }
    }
}
