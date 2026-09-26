//! 在 Windows 10/11 上发送桌面 Toast，并按需注册受控的激活动作。
//!
//! 调用方负责控制通知频率。操作系统禁用通知时不改用模态对话框；API 调用失败
//! 会返回给通知中心，由其决定是否尝试便携版本的消息框回退。
use crate::bindings::*;
use weasel_common::comrt::WinRtApartment;
use windows_strings::HSTRING;

/// Toast 中可由用户确认执行的动作。
///
/// 动作内容由 broker 构造，而不是从不受信任的通知文本中解析。这样既能复用
/// Toast 激活机制，也不会把任意命令或 URI 暴露给其他组件。
pub struct ToastAction {
    /// 显示在 Toast 操作按钮上的文字。
    pub label: &'static str,
    /// 用户单击 Toast 正文或操作按钮后执行的回调。
    pub invoke: Box<dyn Fn() + Send + 'static>,
}

/// 保持 Toast 及其激活处理器存活。
///
/// `ToastNotification::Activated` 的订阅属于通知对象。通知中心持有此值，避免
/// `show` 返回后立即释放对象，导致稍后从操作中心单击通知时收不到回执。
pub struct ActiveToast {
    notification: ToastNotification,
    activation_token: Option<i64>,
}

impl Drop for ActiveToast {
    fn drop(&mut self) {
        if let Some(token) = self.activation_token.take() {
            unsafe {
                // `windows-bindgen` 的扁平投影只公开自动撤销器；这里持有可跨线程
                // 保存的通知对象和原始 token，因此通过其同一 vtable 配对注销。
                let _ = (windows_core::Interface::vtable(&self.notification).RemoveActivated)(
                    windows_core::Interface::as_raw(&self.notification),
                    token,
                );
            }
        }
    }
}

/// 与安装程序现有开始菜单快捷方式一致的通知身份。
///
/// Windows 依赖该身份关联应用通知；发送通知本身不得创建或修改快捷方式。
pub(crate) const APP_ID: &str = "WeaselRS.Broker";

/// 显示一条 Toast，并将 Windows API 错误转换为字符串返回。
///
/// 调用期间初始化单线程 WinRT apartment，并在函数退出（包括错误返回）时配对
/// 反初始化。标题、正文和动作标签会先剔除控制字符、限制长度并转义 XML
/// 特殊字符。返回值必须在动作仍应有效期间保持存活。
pub fn show(
    title: &str,
    message: &str,
    action: Option<ToastAction>,
) -> Result<ActiveToast, String> {
    let _apartment = WinRtApartment::initialize_sta().map_err(|e| e.to_string())?;
    let actions = action.as_ref().map_or_else(String::new, |action| {
        format!(
            "<actions><action content=\"{}\" arguments=\"acknowledge\" activationType=\"foreground\"/></actions>",
            escape_xml(action.label)
        )
    });
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text><text>详情请查看 broker-notifications.*.log。</text></binding></visual>{actions}<audio silent=\"true\"/></toast>",
        escape_xml(title),
        escape_xml(message)
    );
    let document = XmlDocument::new().map_err(|e| e.to_string())?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|e| e.to_string())?;
    let notification =
        ToastNotification::CreateToastNotification(&document).map_err(|e| e.to_string())?;
    let activation_token = if let Some(action) = action {
        Some(
            notification
                .Activated(move |_, _| {
                    (action.invoke)();
                })
                .map_err(|e| e.to_string())?
                .into_token(),
        )
    } else {
        None
    };
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(|e| e.to_string())?;
    notifier.Show(&notification).map_err(|e| e.to_string())?;
    Ok(ActiveToast {
        notification,
        activation_token,
    })
}

/// 清理用于 Toast XML 的文本，避免输入破坏 XML 或注入控制字符。
///
/// 仅保留最多 256 个非控制字符，再转义 XML 保留字符；长度限制按字符而非字节计算。
fn escape_xml(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
