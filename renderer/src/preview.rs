//! 独立的外观预览入口与示例渲染数据。
//!
//! 使用 `weasel-renderer.exe --preview` 时，程序从合成的候选快照渲染当前配置的主题，
//! 并在任务栏显示可手动关闭的预览窗口。主题设置仍从 broker 读取，但此模式不连接
//! server，也不监听实时 renderer 管道；候选项仅用于展示，不会触发真实输入交互。
use crate::theme_api::UiMode;
use weasel_common::message::{RenderItem, RenderRect, RenderSnapshot};

/// 合成预览条带使用的连接所有者编号。
///
/// 该固定编号用于构造有效的演示快照，不对应真实输入会话。
pub(crate) const PREVIEW_OWNER: u64 = 1;

/// 将命令行参数解析为 renderer 的运行模式。
///
/// 不带参数时使用实时模式；仅接受单个 `--preview` 参数进入预览模式。预览主题仍由
/// broker 配置决定，此解析过程不提供主题或输入模式覆盖。
///
/// # 错误
///
/// 参数组合不受支持时返回用法提示。
pub fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<UiMode, String> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(UiMode::Live),
        [arg] if arg == "--preview" => Ok(UiMode::Preview),
        _ => Err("usage: weasel-renderer.exe [--preview]".into()),
    }
}

/// 构造用于展示主题各部分的合成候选快照。
///
/// 快照包含带注释的候选行，并模拟非首页且前后均可翻页的状态，以便主题同时呈现翻页
/// 控件和表情快捷操作面板。锚点仅提供满足可见性校验的有效矩形；预览窗口自行定位，
/// 不依赖该坐标。调用方可将结果交给常规快照校验及展示适配逻辑。
pub fn synthetic_snapshot() -> RenderSnapshot {
    let samples = [
        ("你好", "nihao"),
        ("小狼毫", "xiaolanghao"),
        ("Rime", ""),
        ("中州韻", ""),
    ];
    let items = samples
        .into_iter()
        .map(|(primary, secondary)| RenderItem {
            primary_text: primary.into(),
            secondary_text: secondary.into(),
            enabled: true,
            kind: "candidate".into(),
        })
        .collect();
    RenderSnapshot {
        preedit: None,
        active: true,
        ascii_mode: Some(false),
        visible: true,
        sequence: 1,
        session_id: 1,
        revision: 1,
        // The preview positions itself on the primary monitor and ignores the
        // anchor; it only needs to be valid to satisfy the visibility rule.
        anchor: Some(RenderRect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
            valid: true,
        }),
        items,
        selected_index: 0,
        page_start: 10,
        total_item_count: Some(30),
        can_page_previous: true,
        can_page_next: true,
        token: None,
    }
}

/// 启动独立预览窗口。
///
/// `_mode` 为统一 renderer 入口保留；预览窗口自身负责从 broker 加载主题设置。
///
/// # 错误
///
/// 窗口类、窗口控件或工作线程初始化失败时返回错误文本。
pub fn run(_mode: UiMode) -> Result<(), String> {
    crate::preview_window::run()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_strict() {
        let parse = |args: &[&str]| parse_args(args.iter().map(std::ffi::OsString::from));
        assert_eq!(parse(&[]).unwrap(), UiMode::Live);
        assert_eq!(parse(&["--preview"]).unwrap(), UiMode::Preview);
        assert!(parse(&["--preview", "--preedit"]).is_err());
        assert!(parse(&["--preedit"]).is_err());
        assert!(parse(&["--preview", "--preview"]).is_err());
        assert!(parse(&["--preveiw"]).is_err());
        assert!(parse(&["--preview", "ten"]).is_err());
    }

    #[test]
    fn synthetic_snapshot_is_valid_and_visible() {
        let snapshot = synthetic_snapshot();
        crate::state::validate(&snapshot).unwrap();
        assert!(crate::presentation::is_visible(
            &crate::theme_adapter::view(&snapshot, 1)
        ));
        assert!(snapshot.can_page_previous && snapshot.can_page_next);
        assert_eq!(snapshot.selected_index, 0);
        assert!((snapshot.selected_index as usize) < snapshot.items.len());
    }
}
