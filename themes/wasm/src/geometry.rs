//! 内容空间中的几何计算，与原生合成后端解耦。
//!
//! 这里的尺寸和坐标均以 DIP 表示。阴影外扩量供窗口与画布分配使用，命中测试只判定
//! 圆角内容区域，不包含阴影边缘。
use crate::protocol::PanelStyle;

/// 内容矩形以外的阴影扩展量，单位为 DIP。
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Insets {
    /// 内容左边缘到渲染表面的扩展量。
    pub left: f32,
    /// 内容上边缘到渲染表面的扩展量。
    pub top: f32,
    /// 内容右边缘到渲染表面的扩展量。
    pub right: f32,
    /// 内容下边缘到渲染表面的扩展量。
    pub bottom: f32,
}

/// 根据面板阴影参数计算内容四周需要预留的 DIP 空间。
///
/// 半径非正或阴影颜色完全透明时返回零扩展；否则按半径三倍估算阴影范围，并结合
/// 偏移分别计算四边，结果不小于零。该估算应与画布的阴影裁剪和窗口定位共用。
pub(crate) fn insets(style: &PanelStyle) -> Insets {
    if style.shadow_radius <= 0.0 || style.color >> 24 == 0 {
        return Insets::default();
    }
    let spread = style.shadow_radius * 3.0;
    Insets {
        left: (spread - style.offset_x).max(0.0),
        top: (spread - style.offset_y).max(0.0),
        right: (spread + style.offset_x).max(0.0),
        bottom: (spread + style.offset_y).max(0.0),
    }
}

/// 判断 DIP 点是否落在内容区域的圆角矩形内。
///
/// 矩形采用左闭右开、上闭下开的边界；半径限制在零到短边一半之间。无效或反向尺寸
/// 不会命中。此函数不计入阴影外扩区域。
pub(crate) fn hit_content(x: f32, y: f32, width: f32, height: f32, radius: f32) -> bool {
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return false;
    }
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let dx = x - x.clamp(r, width - r);
    let dy = y - y.clamp(r, height - r);
    dx * dx + dy * dy <= r * r
}
