//! 不创建窗口的主题示例。设置 `"theme": "void"` 即可选用。
//! 本例故意不显示候选、也不产生点击事件，输入和键盘选词仍由原有链路处理。
//!
//! 新主题可复制此模块，修改 Factory 的名称，并在 backend::theme_candidates、
//! broker 的主题校验和 weasel.schema.json 中注册名称。

use crate::theme_api::{
    CandidateView, EventSink, ThemeBackend, ThemeCapabilities, ThemeFactory, UiMode,
};
use weasel_common::settings::ConfigSnapshot;

/// Factory 是无窗口资源的注册对象，可以跨线程查询元数据。
pub struct Factory;

impl ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        "void"
    }

    fn capabilities(&self) -> ThemeCapabilities {
        // 只有真的能显示输入文本的主题才能声明 preedit=true。
        // false 会使 server 回退到宿主内联显示，即使 inline_preedit=false。
        // CANDIDATES_ONLY 表示不支持额外的 preedit 能力，并不强制绘制候选。
        ThemeCapabilities::CANDIDATES_ONLY
    }

    fn default_settings(&self) -> Result<serde_json::Value, String> {
        // 实际主题可用 include_str!("config.json") + serde_json::from_str。
        // 返回主题自身的对象，不要再次包裹 themeSettings 或主题名。
        // runtime 会将安装/用户配置的 themeSettings.void 覆盖到此对象上。
        Ok(serde_json::json!({}))
    }

    fn create(&self, _mode: UiMode, settings: &ConfigSnapshot) -> crate::theme_api::ThemeCreation {
        // ThemeCreation carries notices even if backend initialization fails.
        // Themes return data only: renderer owns logging and user notification.
        // Runtime notices can be returned by ThemeBackend::take_notices().
        (|| -> Result<Box<dyn ThemeBackend>, String> {
            // settings 是合并后的本地只读快照，不需要阻塞 UI 线程请求配置。
            // 实际主题可换成自己的 Deserialize 配置类型，在这里检查参数。
            let _options: serde_json::Map<String, serde_json::Value> =
                settings.theme_settings(self.name())?;

            // create 在 UI 线程执行。窗口、绘图和 COM 资源应在这里创建，
            // 并由 backend 持有和在 Drop 中释放；不要放进全局或 Factory。
            // 初始化失败返回 Err，runtime 才能按注册顺序尝试其他主题。
            // Live 窗口通常不激活、不抢焦点；Preview 的控制窗口由 runtime 提供。
            // void 在两种模式下都不创建主题窗口。
            Ok(Box::new(VoidBackend))
        })()
        .into()
    }
}

struct VoidBackend;

impl ThemeBackend for VoidBackend {
    fn render(&mut self, _view: &CandidateView, _events: &EventSink) -> Result<(), String> {
        // view 是完整显示快照：候选、选中项、翻页状态和可选 preedit。
        // anchor 使用物理屏幕像素（允许负坐标）；不要把它当成 DIP 再缩放。
        // 有窗口的主题应使用 presentation 的定位工具，并单独按 DPI 缩放尺寸。
        // 仅位置变化时，可用 theme_api::same_content 避免重建文字布局。
        //
        // 实际主题应克隆并保存本次 events；点击第 i 个候选时调用：
        // events.send(UiAction::ItemInvoked(i));
        // 翻页使用 NavigatePrevious / NavigateNext，且遵守 view 中的可用状态。
        // 不要在 render 中主动发送事件，也不要把旧控件事件绑到新快照的 sink；
        // sink 已绑定本次输入上下文，通信层负责验证并路由到 server。
        Ok(())
    }

    fn hide(&mut self) {
        // 实际主题应隐藏所有主题窗口、取消鼠标捕获/按下状态，释放旧事件 sink。
        // 不要在这里取消 Rime composition 或修改宿主文本。
    }

    fn refresh_appearance(&mut self) -> Result<(), String> {
        // 系统外观变化时清理颜色/字体等缓存；不要自行显示已隐藏的窗口。
        // runtime 决定当前快照是否仍有所有权、是否需要重新绘制。
        Ok(())
    }

    fn pre_translate(
        &mut self,
        _message: &crate::bindings::Windows::Win32::MSG,
    ) -> Result<bool, String> {
        // 仅在 toolkit 确实消费消息时返回 true；普通 Win32/D2D 主题一般为 false。
        Ok(false)
    }

    fn check_health(&mut self) -> Result<(), String> {
        // 窗口回调不能向外 unwind；实际主题可记录回调错误并在此返回 Err。
        // 无额外健康状态的主题可省略本方法，使用 trait 默认实现。
        Ok(())
    }
}
