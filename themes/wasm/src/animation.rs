//! WASM 唤醒请求：事件内收集，成功后合并进窗口单次计时器。
//! 不创建常驻轮询；较晚请求不能推迟已排队帧。
#[derive(Clone, Copy, Debug, Default)]
pub struct WakeRequest {
    pub cancel: bool,
    pub deadline: Option<f64>,
}
impl WakeRequest {
    pub fn request(&mut self, deadline: f64) {
        self.deadline = Some(self.deadline.map_or(deadline, |old| old.min(deadline)));
    }
    pub fn cancel(&mut self) {
        self.cancel = true;
        self.deadline = None;
    }
    pub fn merge(self, pending: Option<f64>) -> Option<f64> {
        let pending = if self.cancel { None } else { pending };
        match (pending, self.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    /// 同一窗口处理内先后两个成功回调的请求，保留取消语义。
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
