//! 汇总 WASM 事件产生的唤醒请求，并与窗口的单次计时器合并。
//!
//! 请求仅在事件回调期间暂存，成功提交后由窗口安排一次唤醒；不创建常驻
//! 轮询。多个请求取最早期限，后来的请求不会推迟已经排队的帧，取消则按
//! 回调先后顺序作用于之前待处理的期限。
/// 一个事件回调累积的唤醒与取消意图。
#[derive(Clone, Copy, Debug, Default)]
pub struct WakeRequest {
    /// 本次累积是否请求取消先前排队的唤醒。
    pub cancel: bool,
    /// 请求的最早绝对期限（毫秒时钟）；`None` 表示没有新增期限。
    pub deadline: Option<f64>,
}
impl WakeRequest {
    /// 添加唤醒期限；同一回调中的多次请求保留较早者。
    pub fn request(&mut self, deadline: f64) {
        self.deadline = Some(self.deadline.map_or(deadline, |old| old.min(deadline)));
    }
    /// 标记取消并清除本回调此前累积的期限。
    pub fn cancel(&mut self) {
        self.cancel = true;
        self.deadline = None;
    }
    /// 将本回调请求应用到窗口已排队期限上。
    ///
    /// 取消会先清除旧期限；若本回调随后又请求唤醒，则该新期限仍可生效。
    /// 最终合并始终选择现存期限中的最早值。
    pub fn merge(self, pending: Option<f64>) -> Option<f64> {
        let pending = if self.cancel { None } else { pending };
        match (pending, self.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    /// 同一窗口处理内先后两个成功回调的请求，保留取消语义。
    ///
    /// 右侧请求视为时间上较晚；较晚的取消清除较早期限，较晚的新期限则与
    /// 较早请求取最小值。该合并顺序与事件提交顺序一致。
    pub fn then(self, next: Self) -> Self {
        Self {
            cancel: self.cancel || next.cancel,
            deadline: next.merge(self.deadline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_coalesce_without_postponing_and_cancel_is_ordered() {
        let mut request = WakeRequest::default();
        request.request(120.0);
        request.request(110.0);
        assert_eq!(request.merge(Some(100.0)), Some(100.0));
        request.cancel();
        assert_eq!(request.merge(Some(100.0)), None);
        request.request(150.0);
        assert_eq!(request.merge(Some(100.0)), Some(150.0));
    }
}
