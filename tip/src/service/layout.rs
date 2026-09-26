//! 查询组合锚点的 TSF 布局，并向渲染端发送带上下文和修订标识的几何信息。
//!
//! 布局通知先合并为一个异步只读编辑会话；执行时重新读取当前组合和视图，
//! 并在发出结果前复核上下文代次、编辑修订及组合身份，丢弃过期几何数据。
use super::*;
use crate::bindings::{
    GA_ROOT, GetAncestor, LogicalToPhysicalPointForPerMonitorDPI, POINT, TF_ES_ASYNC,
};
use weasel_common::message::ContextToken;

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
    /// 待执行票据及其最新上下文令牌。
    pending: Option<(u64, ContextToken)>,
}
impl LayoutSchedule {
    /// 有待处理会话时更新其令牌，否则创建新票据并请求排队一个会话。
    fn request(&mut self, token: ContextToken) -> Option<u64> {
        if let Some((_, latest)) = &mut self.pending {
            *latest = token;
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.pending = Some((self.serial, token));
        Some(self.serial)
    }
    /// 仅由对应票据取走最新令牌，过期会话不能消费后续请求。
    fn take(&mut self, ticket: u64) -> Option<ContextToken> {
        if self.pending.as_ref().is_some_and(|(id, _)| *id == ticket) {
            self.pending.take().map(|(_, token)| token)
        } else {
            None
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
        let Some(tid) = *self.lock(&self.keystroke_client_id)? else {
            return Ok(());
        };
        let token = state.token()?;
        let Some(ticket) = self.lock(&state.layout)?.request(token) else {
            return Ok(());
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
        if result.is_err() {
            self.lock(&state.layout)?.take(ticket);
        }
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
            let Some(token) = self
                .state
                .layout
                .try_lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .take(self.ticket)
            else {
                return Ok(());
            };
            if !self.state.matches(Some(&token))?
                || self.generation.load(Ordering::Acquire) != self.requested_generation
            {
                return Ok(());
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
                || !self.state.matches(Some(&token))?
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
                    token: Some(token),
                    // Some(0) means this TIP has not applied the host edit yet;
                    // None is reserved for older TIPs without this field.
                    revision: Some(applied_revision),
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
        let first = schedule.request(ContextToken::default()).unwrap();
        let latest = ContextToken {
            generation: 2,
            ..Default::default()
        };
        for _ in 0..1000 {
            assert!(schedule.request(latest.clone()).is_none());
        }
        assert_eq!(schedule.take(first), Some(latest));
        let second = schedule.request(ContextToken::default()).unwrap();
        assert!(schedule.take(first).is_none());
        assert!(schedule.take(second).is_some());
    }
}
