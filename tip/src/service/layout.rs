//! 查询组合锚点的 TSF 布局，并向渲染端发送带上下文和修订标识的几何信息。
//!
//! 布局通知先合并为一个异步只读编辑会话；执行时重新读取当前组合和视图，
//! 并在发出结果前复核上下文代次、编辑修订及组合身份，丢弃过期几何数据。
use super::*;
use crate::bindings::{
    GA_ROOT, GetAncestor, LogicalToPhysicalPointForPerMonitorDPI, POINT, TF_AE_START, TF_ES_ASYNC,
};
use std::time::{Duration, Instant};
use weasel_common::message::ContextToken;

/// TIP 不应在 Server 已放弃提示后继续重试定位。
const MODE_INDICATOR_LAYOUT_LIFETIME: Duration = Duration::from_millis(800);

#[derive(Clone)]
/// 一次非 composition 模式提示所需的定位信息。
struct ModeIndicatorLayout {
    /// 目标上下文身份。
    token: ContextToken,
    /// 与 Server 待展示请求匹配的不透明标识。
    request_id: u64,
    /// 本地重试截止时间；只由布局事件驱动，不启动轮询。
    expires_at: Instant,
}

/// 只读布局会话的用途；模式提示与组合布局不会共享版本语义。
enum LayoutPurpose {
    /// 为现有 composition 查询候选窗锚点。
    Composition(ContextToken),
    /// 为输入前的短暂模式提示查询当前选择／插入点。
    ModeIndicator(ModeIndicatorLayout),
}

/// 返回用于定位候选窗的窄范围，避免尚未完成布局的其余组合文字影响锚点。
/// 非空组合取起始处一个字符；空组合（外部预编辑）则保持折叠在插入点。
fn composition_target_range(composition: &ITfComposition, ec: TfEditCookie) -> Result<ITfRange> {
    let composition_range = unsafe { composition.GetRange()? };
    let empty = unsafe { composition_range.IsEmpty(ec)? }.as_bool();
    let target_range = unsafe { composition_range.Clone()? };
    let hr = unsafe { target_range.Collapse(ec, TF_ANCHOR_START) };
    if hr.is_err() {
        return Err(Error::from_hresult(hr));
    }

    if empty {
        return Ok(target_range);
    }

    let mut moved = 0;
    let hr = unsafe { target_range.ShiftEnd(ec, 1, &mut moved, ptr::null()) };
    if hr.is_err() {
        return Err(Error::from_hresult(hr));
    }
    Ok(target_range)
}

/// 合并待执行的 TSF 只读会话；执行前到达的通知只刷新其上下文令牌。
#[derive(Default)]
pub(super) struct LayoutSchedule {
    /// 用于区分先后排队的只读会话。
    serial: u64,
    /// 待执行票据及其用途。
    pending: Option<(u64, LayoutPurpose)>,
    /// `GetTextExt` 暂时无布局时，等待下一次布局通知重试的提示。
    retry_indicator: Option<ModeIndicatorLayout>,
}
impl LayoutSchedule {
    /// 组合布局优先；若提示查询尚未执行，则复用其票据并改为组合用途。
    fn request_composition(&mut self, token: ContextToken) -> Option<u64> {
        self.retry_indicator = None;
        if let Some((_, purpose)) = &mut self.pending {
            *purpose = LayoutPurpose::Composition(token);
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some((self.serial, LayoutPurpose::Composition(token)));
        Some(self.serial)
    }

    /// 新提示替换尚未执行或等待重试的旧提示；组合查询存在时不抢占它。
    fn request_indicator(&mut self, indicator: ModeIndicatorLayout) -> Option<u64> {
        self.retry_indicator = None;
        if let Some((_, purpose)) = &mut self.pending {
            if matches!(purpose, LayoutPurpose::Composition(_)) {
                return None;
            }
            *purpose = LayoutPurpose::ModeIndicator(indicator);
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some((self.serial, LayoutPurpose::ModeIndicator(indicator)));
        Some(self.serial)
    }

    /// 仅由对应票据取走最新用途，过期会话不能消费后续请求。
    fn take(&mut self, ticket: u64) -> Option<LayoutPurpose> {
        if self.pending.as_ref().is_some_and(|(id, _)| *id == ticket) {
            self.pending.take().map(|(_, purpose)| purpose)
        } else {
            None
        }
    }

    /// 保存一次仅由下一次 TSF 布局通知唤醒的提示重试。
    fn retry_indicator(&mut self, indicator: ModeIndicatorLayout) {
        if Instant::now() < indicator.expires_at && self.pending.is_none() {
            self.retry_indicator = Some(indicator);
        }
    }

    /// 取出尚未过期的提示重试；过期请求直接丢弃。
    fn take_retry_indicator(&mut self) -> Option<ModeIndicatorLayout> {
        self.retry_indicator
            .take()
            .filter(|indicator| Instant::now() < indicator.expires_at)
    }

    /// 失焦或上下文失效时取消所有提示定位工作，不影响组合布局。
    pub(super) fn cancel_indicator(&mut self) {
        self.retry_indicator = None;
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, purpose)| matches!(purpose, LayoutPurpose::ModeIndicator(_)))
        {
            self.pending = None;
        }
    }
}
impl TextService {
    /// 为当前焦点上下文的活动组合安排异步只读布局查询。
    /// TSF 写锁释放后才执行；调用方可显式请求终止后的最终布局探测。
    pub(super) fn request_composition_layout(&self, state: &Arc<ContextState>) -> Result<()> {
        if !self.activated.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
            || self.faulted.load(Ordering::Acquire)
            || *self.lock(&self.focused_context)? != Some(state.id)
            || self.lock(&state.composition)?.is_none()
        {
            return Ok(());
        }
        let token = state.token()?;
        let Some(ticket) = self.lock(&state.layout)?.request_composition(token) else {
            return Ok(());
        };
        let result = self.request_layout_probe(state, ticket);
        if result.is_err() {
            self.lock(&state.layout)?.take(ticket);
        }
        result
    }

    /// 为 Server 明确请求的模式提示安排一次插入点查询。
    ///
    /// 安全、只读、挂起或已经开始组合的上下文不会产生提示；失败只取消本次
    /// 展示，不会隔离输入服务或影响后续按键。
    pub(super) fn request_mode_indicator_layout(
        &self,
        state: &Arc<ContextState>,
        request_id: u64,
    ) -> Result<()> {
        if request_id == 0
            || !self.activated.load(Ordering::Acquire)
            || !state.alive.load(Ordering::Acquire)
            || state.suspended.load(Ordering::Acquire)
            || self.faulted.load(Ordering::Acquire)
            || *self.lock(&self.focused_context)? != Some(state.id)
            || self.lock(&state.composition)?.is_some()
            || self.should_bypass_secure_field(state)?
            || !super::key_event::context_is_writable(unsafe { state.context.GetStatus() })
        {
            return Ok(());
        }
        let indicator = ModeIndicatorLayout {
            token: state.token()?,
            request_id,
            expires_at: Instant::now() + MODE_INDICATOR_LAYOUT_LIFETIME,
        };
        let Some(ticket) = self.lock(&state.layout)?.request_indicator(indicator) else {
            return Ok(());
        };
        if self.request_layout_probe(state, ticket).is_err() {
            self.lock(&state.layout)?.take(ticket);
        }
        Ok(())
    }

    /// 在布局通知到达时重试一次先前遇到无布局状态的提示查询。
    pub(super) fn retry_mode_indicator_layout(&self, state: &Arc<ContextState>) -> Result<()> {
        if self.lock(&state.composition)?.is_some() {
            self.lock(&state.layout)?.cancel_indicator();
            return Ok(());
        }
        let Some(indicator) = self.lock(&state.layout)?.take_retry_indicator() else {
            return Ok(());
        };
        let Some(ticket) = self
            .lock(&state.layout)?
            .request_indicator(indicator.clone())
        else {
            return Ok(());
        };
        if self.request_layout_probe(state, ticket).is_err() {
            let mut layout = self.lock(&state.layout)?;
            layout.take(ticket);
            layout.retry_indicator(indicator);
        }
        Ok(())
    }

    /// 将已登记票据提交给 TSF 的异步只读编辑会话。
    fn request_layout_probe(&self, state: &Arc<ContextState>, ticket: u64) -> Result<()> {
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Err(Error::from_hresult(boundary::E_FAIL));
        };
        let probe: ITfEditSession = LayoutProbe {
            state: state.clone(),
            ticket,
            generation: Arc::clone(&self.generation),
            requested_generation: self.generation.load(Ordering::Acquire),
            _module: ModuleLease::new(),
        }
        .into();
        // Defer until the write lock is released; retain Terminal's explicit
        // post-composition probe even when no layout callback is delivered.
        let result = unsafe {
            state
                .context
                .RequestEditSession(tid, &probe, TF_ES_ASYNC | TF_ES_READ)
        }
        .and_then(|hr| hr.ok());
        result
    }
}
#[implement(ITfEditSession)]
/// 持有布局票据的 TSF 只读会话；回调只发送仍与当前编辑状态相符的结果。
pub(super) struct LayoutProbe {
    /// 持有目标文档上下文，使异步会话可安全读取状态。
    state: Arc<ContextState>,
    /// 本次只读会话在布局调度器中的票据。
    ticket: u64,
    /// 服务激活代次；与请求时的值不同时不发送布局结果。
    generation: Arc<AtomicU64>,
    /// 排队时观察到的服务代次。
    requested_generation: u64,
    /// 保持实现 ITfEditSession 的 COM 模块在回调期间加载。
    _module: ModuleLease,
}
impl Drop for LayoutProbe {
    fn drop(&mut self) {
        // TSF 可能丢弃而不执行会话。
        if let Ok(mut layout) = self.state.layout.try_lock() {
            layout.take(self.ticket);
        }
    }
}
impl ITfEditSession_Impl for LayoutProbe_Impl {
    /// 使用 TSF 读 cookie 查询当前组合几何，并在 RPC 发送前复核其有效性。
    /// 暂时无法取得文本几何时静默放弃本次更新，不影响键入。
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            let Some(purpose) = self
                .state
                .layout
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .take(self.ticket)
            else {
                return Ok(());
            };
            let token = match &purpose {
                LayoutPurpose::Composition(token) => token,
                LayoutPurpose::ModeIndicator(indicator) => &indicator.token,
            };
            if !self.state.matches(Some(token))?
                || self.generation.load(Ordering::Acquire) != self.requested_generation
            {
                return Ok(());
            }
            if let LayoutPurpose::ModeIndicator(indicator) = purpose {
                return self.query_mode_indicator(ec, indicator);
            }
            let Some(composition) = self
                .state
                .composition
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .clone()
            else {
                return Ok(());
            };
            let applied_revision = self.state.applied_layout_revision.load(Ordering::Acquire);
            // Query current geometry, not a range captured before queued edits.
            let view = unsafe { self.state.context.GetActiveView()? };
            let range = composition_target_range(&composition, ec)?;
            let mut rect = RECT::default();
            let mut clipped = BOOL(0);
            if unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }.is_err() {
                // Transient geometry failure must not flood the key/log queue.
                return Ok(());
            }
            let rect = physical_text_rect(&view, rect);
            if self.generation.load(Ordering::Acquire) != self.requested_generation
                || self.state.applied_layout_revision.load(Ordering::Acquire) != applied_revision
                || !self.state.matches(Some(token))?
                || self
                    .state
                    .composition
                    .try_lock()
                    .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                    .as_ref()
                    != Some(&composition)
            {
                return Ok(());
            }
            self.state
                .rpc
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .send_layout_update(LayoutUpdate {
                    session_id: self.state.id,
                    token: Some(token.clone()),
                    // Some(0) means this TIP has not applied the host edit yet;
                    // None is reserved for older TIPs without this field.
                    revision: Some(applied_revision),
                    mode_indicator_request_id: None,
                    anchor: Some(RenderRect {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                        // An empty TSF composition still has a caret-height rect.
                        valid: rect.right >= rect.left && rect.bottom > rect.top,
                    }),
                });
            Ok(())
        })
    }
}

impl LayoutProbe_Impl {
    /// 查询当前选择的活动端；无布局时保留到下一次 `OnLayoutChange` 重试。
    fn query_mode_indicator(&self, ec: TfEditCookie, indicator: ModeIndicatorLayout) -> Result<()> {
        if Instant::now() >= indicator.expires_at
            || self.state.secure_bypass_active.load(Ordering::Acquire)
            || self
                .state
                .composition
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .is_some()
            || !super::key_event::context_is_writable(unsafe { self.state.context.GetStatus() })
        {
            return Ok(());
        }
        let mut selection = TF_SELECTION::default();
        let mut fetched = 0;
        let selected = unsafe {
            let result = self.state.context.GetSelection(
                ec,
                bindings::TF_DEFAULT_SELECTION,
                1,
                &mut selection,
                &mut fetched,
            );
            let selected = std::mem::ManuallyDrop::take(&mut selection.range);
            if result.is_err() || fetched != 1 {
                return Ok(());
            }
            selected
        };
        let Some(range) = selected else {
            return Ok(());
        };
        let anchor = if selection.style.ase == TF_AE_START {
            TF_ANCHOR_START
        } else {
            TF_ANCHOR_END
        };
        if unsafe { range.Collapse(ec, anchor) }.is_err() {
            return Ok(());
        }
        let Ok(view) = (unsafe { self.state.context.GetActiveView() }) else {
            return Ok(());
        };
        let mut rect = RECT::default();
        let mut clipped = BOOL(0);
        if unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }.is_err() {
            self.state
                .layout
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .retry_indicator(indicator);
            return Ok(());
        }
        let rect = physical_text_rect(&view, rect);
        if Instant::now() >= indicator.expires_at || !self.state.matches(Some(&indicator.token))? {
            return Ok(());
        }
        self.state
            .rpc
            .try_lock()
            .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
            .send_layout_update(LayoutUpdate {
                session_id: self.state.id,
                token: Some(indicator.token),
                revision: None,
                mode_indicator_request_id: Some(indicator.request_id),
                anchor: Some(RenderRect {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                    valid: rect.right >= rect.left && rect.bottom > rect.top,
                }),
            });
        Ok(())
    }
}

/// 将文本视图返回的宿主 DPI 坐标转换为渲染端使用的物理屏幕像素。
/// 转换以源视图所属根窗口为准；任一角点转换失败时保留原矩形，避免混用坐标系。
fn physical_text_rect(view: &ITfContextView, rect: RECT) -> RECT {
    unsafe {
        let Ok(window) = view.GetWnd() else {
            return rect;
        };
        if window.0.is_null() {
            return rect;
        }
        let root = GetAncestor(window, GA_ROOT as u32);
        if root.0.is_null() {
            return rect;
        }
        let mut start = POINT {
            x: rect.left,
            y: rect.top,
        };
        let mut end = POINT {
            x: rect.right,
            y: rect.bottom,
        };
        // Commit both corners together. Geometry conversion failure should not
        // interrupt typing or produce a rectangle mixing coordinate spaces.
        if !LogicalToPhysicalPointForPerMonitorDPI(Some(root), &mut start).as_bool()
            || !LogicalToPhysicalPointForPerMonitorDPI(Some(root), &mut end).as_bool()
        {
            return rect;
        }
        RECT {
            left: start.x,
            top: start.y,
            right: end.x,
            bottom: end.y,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalesces_and_old_drop_cannot_cancel_next_probe() {
        let mut schedule = LayoutSchedule::default();
        let first = schedule
            .request_composition(ContextToken::default())
            .unwrap();
        let latest = ContextToken {
            generation: 2,
            ..Default::default()
        };
        for _ in 0..1000 {
            assert!(schedule.request_composition(latest.clone()).is_none());
        }
        assert!(matches!(
            schedule.take(first),
            Some(LayoutPurpose::Composition(token)) if token == latest
        ));
        let second = schedule
            .request_composition(ContextToken::default())
            .unwrap();
        assert!(schedule.take(first).is_none());
        assert!(schedule.take(second).is_some());
    }
}
