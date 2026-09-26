//! 查询系统背景明暗，并将系统外观变化转发给主题线程。
//!
//! 本模块不依赖主题使用的绘制工具包。订阅对象必须在需要接收通知期间保持存活；回调只
//! 向指定线程投递消息，不在系统设置回调中直接操作主题 UI。
use crate::bindings::{UIColorType, UISettings};

/// 根据系统背景色判断当前是否为深色外观。
///
/// 系统设置创建或取色失败时返回 `false`，即采用浅色外观作为回退。
pub fn is_dark() -> bool {
    UISettings::new()
        .and_then(|settings| settings.GetColorValue(UIColorType::Background))
        .is_ok_and(|color| is_dark_rgb(color.R, color.G, color.B))
}

fn is_dark_rgb(r: u8, g: u8, b: u8) -> bool {
    299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000
}

/// 持有系统外观变化订阅及其设置对象。
///
/// 保留此值即可维持订阅；销毁时先撤销回调，再释放设置对象。
pub struct AppearanceSubscription {
    // Revoke the callback before releasing settings.
    _revoker: Option<windows_core::EventRevoker>,
    _settings: UISettings,
}

impl AppearanceSubscription {
    /// 创建订阅，并在外观变化时向 `thread_id` 投递 `message`。
    ///
    /// 无法创建系统设置对象时返回 `None`。事件注册失败不影响设置对象的创建，此时返回
    /// 的订阅仍有效但不会收到通知；消息投递失败会被忽略，调用方可在自己的线程轮询外观。
    pub fn new(thread_id: u32, message: u32) -> Option<Self> {
        use crate::bindings::{LPARAM, PostThreadMessageW, WPARAM};
        let settings = UISettings::new().ok()?;
        let revoker = settings
            .ColorValuesChanged(move |_, _| unsafe {
                let _ = PostThreadMessageW(thread_id, message, WPARAM(0), LPARAM(0));
            })
            .ok();
        Some(Self {
            _revoker: revoker,
            _settings: settings,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn classifies_system_background() {
        assert!(super::is_dark_rgb(28, 28, 28));
        assert!(!super::is_dark_rgb(248, 248, 248));
    }
}
