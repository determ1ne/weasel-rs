//! 在 TSF 读编辑会话中检查当前选区的输入范围，并将密码字段状态保存在文档上下文中。
use super::*;
use crate::bindings::{
    CoTaskMemFree, GUID_COMPARTMENT_EMPTYCONTEXT, GUID_COMPARTMENT_KEYBOARD_DISABLED,
    GUID_PROP_INPUTSCOPE, IS_ALPHANUMERIC_PIN, IS_ALPHANUMERIC_PIN_SET, IS_NUMERIC_PASSWORD,
    IS_NUMERIC_PIN, IS_PASSWORD, IS_PRIVATE, ITfCompartmentMgr, InputScopeManual,
    TF_DEFAULT_SELECTION, VariantClear,
};
use weasel_common::message::ContextAction;

/// 宿主未提供、尚未准备好或暂时无法读取 InputScope。
pub(super) const UNAVAILABLE: u8 = 0;
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
    /// 唯一有效的待处理探测票据，以及它是否已提交给 TSF。
    pending: Option<(u64, bool)>,
}

impl SecureProbeSchedule {
    /// 将探测排入 TIP 消息窗口；已有请求尚未执行时合并本次请求。
    fn queue(&mut self) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some((self.serial, false));
        Some(self.serial)
    }

    /// 取得尚未提交给 TSF 的 ticket ，并阻止同一请求被重复提交。
    fn submit(&mut self) -> Option<u64> {
        let (ticket, submitted) = self.pending.as_mut()?;
        if *submitted {
            return None;
        }
        *submitted = true;
        Some(*ticket)
    }

    /// 仅消费当前 ticket ，防止迟到的旧会话清除新请求。
    fn take(&mut self, ticket: u64) -> bool {
        if self.pending.is_some_and(|pending| pending.0 == ticket) {
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
        (*inner.Anonymous.punkVal).clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecureFieldState {
    Unavailable,
    Normal,
    Secure,
}

impl SecureFieldState {
    fn stored(self) -> u8 {
        match self {
            Self::Unavailable => UNAVAILABLE,
            Self::Normal => NORMAL,
            Self::Secure => SECURE,
        }
    }
}

/// 读取当前选区的应用输入范围属性。
///
/// 查询以选区 range 实际所属的 context 为准。
fn query_secure_field(context: &ITfContext, ec: TfEditCookie) -> SecureFieldState {
    let Ok(Some(range)) = selected_range(context, ec) else {
        return SecureFieldState::Unavailable;
    };
    let Ok(property_context) = (unsafe { range.GetContext() }) else {
        return SecureFieldState::Unavailable;
    };
    let Ok(property): Result<ITfReadOnlyProperty> =
        (unsafe { property_context.GetAppProperty(&GUID_PROP_INPUTSCOPE) })
    else {
        return SecureFieldState::Unavailable;
    };
    let Ok(mut value) = (unsafe { property.GetValue(ec, &range) }) else {
        return SecureFieldState::Unavailable;
    };
    let unknown = input_scope_unknown(&mut value);
    unsafe {
        let _ = VariantClear(&mut value);
    }
    let Some(unknown) = unknown else {
        return SecureFieldState::Unavailable;
    };
    let Ok(input_scope) = unknown.cast::<ITfInputScope>() else {
        return SecureFieldState::Unavailable;
    };
    let mut scopes = std::ptr::null_mut();
    let mut count = 0;
    if unsafe { input_scope.GetInputScopes(&mut scopes, &mut count) }.is_err() {
        return SecureFieldState::Unavailable;
    }
    let scopes = ScopeBuffer(scopes);
    if count > MAX_INPUT_SCOPES || (count != 0 && scopes.0.is_null()) {
        return SecureFieldState::Unavailable;
    }
    let values = if count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(scopes.0, count as usize) }
    };
    if values.iter().any(|scope| {
        matches!(
            *scope,
            IS_PASSWORD
                | IS_PRIVATE
                | IS_NUMERIC_PASSWORD
                | IS_NUMERIC_PIN
                | IS_ALPHANUMERIC_PIN
                | IS_ALPHANUMERIC_PIN_SET
        )
    }) {
        SecureFieldState::Secure
    } else {
        SecureFieldState::Normal
    }
}

/// 读取 context 级 TSF compartment 的布尔值；缺失和格式异常按未设置处理。
fn context_compartment_flag(context: &ITfContext, guid: &GUID) -> bool {
    let Ok(compartments) = context.cast::<ITfCompartmentMgr>() else {
        return false;
    };
    let Ok(compartment) = (unsafe { compartments.GetCompartment(guid) }) else {
        return false;
    };
    let Ok(mut value) = (unsafe { compartment.GetValue() }) else {
        return false;
    };
    let enabled = unsafe {
        let inner = &*value.Anonymous.Anonymous;
        inner.vt == VARTYPE(VT_I4 as u16) && inner.Anonymous.lVal != 0
    };
    unsafe {
        let _ = VariantClear(&mut value);
    }
    enabled
}

/// 宿主明确要求该 context 不接受键盘 TIP 时始终直通。
fn context_rejects_tip(context: &ITfContext) -> bool {
    context_compartment_flag(context, &GUID_COMPARTMENT_KEYBOARD_DISABLED)
        || context_compartment_flag(context, &GUID_COMPARTMENT_EMPTYCONTEXT)
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
            let secure = query_secure_field(&self.state.context, ec);
            let service = unsafe { &*self.service };
            service.set_secure_field(&self.state, secure);
            Ok(())
        })
    }
}

impl TextService {
    /// 将密码字段探测投递到 TIP 消息窗口。
    ///
    /// `TF_ES_ASYNCDONTCARE` 仍允许 TSF 当场执行编辑会话，因此不能直接从
    /// `OnPushContext`、`OnSetFocus` 或 `OnEndEdit` 等宿主回调中调用
    /// `RequestEditSession`。先经过窗口消息队列，保证读取应用属性时原回调已经返回。
    pub(super) fn queue_secure_field_probe(&self, state: &Arc<ContextState>) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || !state.alive.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(ticket) = self.lock(&state.secure_probe)?.queue() else {
            return Ok(());
        };
        let hwnd = self
            .lock(&self.update_window)?
            .as_ref()
            .map(|window| window.hwnd);
        let posted = hwnd.is_some_and(|hwnd| unsafe {
            bindings::PostMessageW(
                Some(hwnd),
                update_window::UPDATE_MESSAGE,
                WPARAM(0),
                LPARAM(0),
            )
            .as_bool()
        });
        if !posted {
            self.lock(&state.secure_probe)?.take(ticket);
        }
        Ok(())
    }

    /// 在 TIP 自有窗口消息中把已排队的探测提交给 TSF。
    pub(super) fn dispatch_secure_field_probes(&self, owner: &IUnknown) -> Result<()> {
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        let states = self.lock(&self.contexts)?.clone();
        for state in states {
            if !state.alive.load(Ordering::Acquire) || state.suspended.load(Ordering::Acquire) {
                continue;
            }
            let Some(ticket) = self.lock(&state.secure_probe)?.submit() else {
                continue;
            };
            let probe: ITfEditSession = SecureFieldProbe {
                service: self,
                state: state.clone(),
                ticket,
                generation: state.generation.load(Ordering::Acquire),
                _owner: owner.clone(),
                _module: ModuleLease::new(),
            }
            .into();
            let result = unsafe {
                state
                    .context
                    .RequestEditSession(tid, &probe, TF_ES_ASYNCDONTCARE | TF_ES_READ)
            }
            .and_then(|session| session.ok());
            if result.is_err() {
                self.lock(&state.secure_probe)?.take(ticket);
            }
        }
        Ok(())
    }

    /// 原子更新字段状态；只有状态实际变化时才通知更新窗口重新协调输入。
    fn set_secure_field(&self, state: &ContextState, secure: SecureFieldState) {
        let next = secure.stored();
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
            || context_rejects_tip(&state.context)
    }

    /// 读取当前 RPC 连接代次对应的用户安全字段策略。
    fn secure_policy_allows_rime(&self, state: &ContextState) -> Result<bool> {
        let epoch = self.lock(&state.rpc)?.connection_epoch();
        Ok(epoch != 0
            && self.secure_policy_epoch.load(Ordering::Acquire) == epoch
            && self.allow_rime_in_secure_fields.load(Ordering::Acquire))
    }

    /// 只有宿主未禁用 TIP 且当前连接明确允许时，才允许在安全字段中使用 Rime。
    pub(super) fn allow_rime_for_secure_field(&self, state: &ContextState) -> Result<bool> {
        if context_rejects_tip(&state.context) {
            return Ok(false);
        }
        self.secure_policy_allows_rime(state)
    }

    /// 判断安全策略是否要求绕过 Rime。
    ///
    /// context 禁用 compartment 不受用户密码字段选项覆盖；InputScope 不可用
    /// 则不阻断输入，等待后续焦点或编辑通知重新探测。
    pub(super) fn should_bypass_secure_field(&self, state: &ContextState) -> Result<bool> {
        if context_rejects_tip(&state.context) {
            return Ok(true);
        }
        Ok(!self.secure_policy_allows_rime(state)?
            && (self.secure_mode.load(Ordering::Acquire)
                || state.secure_field.load(Ordering::Acquire) == SECURE))
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
        // InputScope 不可用时不影响普通输入；仅已确认的安全字段或宿主明确
        // 禁用 TIP 时才进入绕过状态。
        let rejected = context_rejects_tip(&state.context);
        let secure = rejected
            || self.secure_mode.load(Ordering::Acquire)
            || state.secure_field.load(Ordering::Acquire) == SECURE;
        let bypass = secure && (rejected || !self.secure_policy_allows_rime(&state)?);
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
