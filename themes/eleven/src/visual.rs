//! eleven 主题的候选项、快捷操作视觉树及系统配色资源。
//!
//! 所有 XAML 对象在所属 UI STA 使用；画刷、字体和系统调色板按主题实例缓存，
//! 外观刷新时统一失效。事件撤销句柄由渲染调用方持有，以便销毁或替换视觉树。

use crate::theme_api::{CandidateView, UiAction};
use windows_core::HSTRING;
use windows_version::OsVersion;

/// 根据系统版本选择包含所需字形的图标字体，兼顾旧版 Windows 10。
fn icon_font_for_version(version: OsVersion) -> &'static str {
    const WINDOWS_10: OsVersion = OsVersion::new(10, 0, 0, 0);
    const WINDOWS_11: OsVersion = OsVersion::new(10, 0, 0, 22_000);
    if version >= WINDOWS_10 && version < WINDOWS_11 {
        "Segoe MDL2 Assets"
    } else {
        "Segoe Fluent Icons"
    }
}

/// 获取当前进程选定的图标字体；进程内只检测并缓存一次。
fn icon_font() -> &'static str {
    static FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FONT.get_or_init(|| icon_font_for_version(OsVersion::current()))
}

/// 返回与对应系统图标字体匹配的表情面板入口字形。
fn emoji_glyph_for_version(version: OsVersion) -> &'static str {
    if icon_font_for_version(version) == "Segoe MDL2 Assets" {
        "\u{E76E}" // Emoji2: available in older Windows 10 MDL2 fonts.
    } else {
        "\u{F6B8}" // ExpressiveInputEntry in Segoe Fluent Icons.
    }
}

#[cfg(test)]
mod icon_font_tests {
    use super::*;

    #[test]
    fn windows_10_uses_mdl2() {
        for build in [18362, 19041, 19045, 21999] {
            assert_eq!(
                icon_font_for_version(OsVersion::new(10, 0, 0, build)),
                "Segoe MDL2 Assets"
            );
            assert_eq!(
                emoji_glyph_for_version(OsVersion::new(10, 0, 0, build)),
                "\u{E76E}"
            );
        }
    }

    #[test]
    fn windows_11_and_later_keep_fluent() {
        for build in [22000, 22621, 26100] {
            assert_eq!(
                icon_font_for_version(OsVersion::new(10, 0, 0, build)),
                "Segoe Fluent Icons"
            );
            assert_eq!(
                emoji_glyph_for_version(OsVersion::new(10, 0, 0, build)),
                "\u{F6B8}"
            );
        }
        assert_eq!(
            icon_font_for_version(OsVersion::new(11, 0, 0, 0)),
            "Segoe Fluent Icons"
        );
    }
}

use crate::bindings::{
    AcrylicBackgroundSource, AcrylicBrush, Border, Color, CornerRadius, ElementTheme, FontFamily,
    Grid, HorizontalAlignment, Orientation, SolidColorBrush, StackPanel, TextAlignment, TextBlock,
    Thickness, UIColorType, UISettings, VerticalAlignment, Visibility,
};

/// eleven 候选窗的视觉配置和 UI 资源缓存。
///
/// 内部 `RefCell`/`Cell` 缓存允许渲染辅助方法通过共享引用复用 COM 对象；因此
/// 实例只应在创建它的 UI STA 上串行访问。`refresh` 会使外观相关缓存失效，
/// 不销毁由调用方持有的 XAML 控件或事件撤销句柄。
pub struct CandidateTheme {
    /// 当前主题的字体、颜色配置及共享告警缓冲区。
    config: super::config::ThemeConfig,
    /// 按 RGBA 键缓存的纯色画刷，避免重复创建 WinRT 画刷对象。
    brushes: std::cell::RefCell<Vec<([u8; 4], SolidColorBrush)>>,
    /// 当前系统/配置合成的调色板；外观刷新后清空。
    palette: std::cell::Cell<Option<Palette>>,
    /// 已创建的候选字体族对象；外观刷新后清空。
    font: std::cell::RefCell<Option<FontFamily>>,
}

/// 中英文模式提示的独立 XAML 子树。
///
/// 该视图与候选面板共享同一个 Island 和宿主 HWND，但拥有自己的布局树，避免
/// 候选使用的横向 `StackPanel`、列定义和最小宽度影响正方形提示的测量结果。
pub struct ModeIndicatorVisual {
    /// 覆盖在候选面板上的提示根节点。
    panel: Grid,
    /// 自然尺寸的“中”或“英”文字；不设置固定高度，以便真正垂直居中。
    label: TextBlock,
    /// 文字下方的主题强调色横条。
    accent: Border,
}

impl ModeIndicatorVisual {
    /// 创建并连接提示子树，初始保持折叠。
    pub fn new() -> windows_core::Result<Self> {
        let panel = Grid::new()?;
        let content = StackPanel::new()?;
        let label = TextBlock::new()?;
        let accent = Border::new()?;

        content.SetOrientation(Orientation::Vertical)?;
        content.SetHorizontalAlignment(HorizontalAlignment::Center)?;
        content.SetVerticalAlignment(VerticalAlignment::Center)?;
        content.SetSpacing(0.0)?;
        let children = content.Children()?;
        children.Append(&label)?;
        children.Append(&accent)?;
        panel.Children()?.Append(&content)?;
        panel.SetVisibility(Visibility::Collapsed)?;

        Ok(Self {
            panel,
            label,
            accent,
        })
    }

    /// 返回可附加到外层展示容器的根元素。
    pub fn panel(&self) -> &Grid {
        &self.panel
    }
}

impl CandidateTheme {
    /// 取出主题配置加载或渲染期间累积的告警。
    pub fn take_notices(&self) -> Vec<crate::theme_api::ThemeNotice> {
        self.config.take_notices()
    }

    /// 创建具有空 UI 资源缓存的主题状态。
    pub fn new(config: super::config::ThemeConfig) -> Self {
        Self {
            config,
            brushes: Default::default(),
            palette: Default::default(),
            font: Default::default(),
        }
    }
    /// 使画刷、配色和字体缓存失效，供系统外观变化后重新读取并建立资源。
    pub fn refresh(&mut self) {
        self.brushes.get_mut().clear();
        self.palette.set(None);
        *self.font.get_mut() = None;
    }
    /// 返回指定颜色的缓存画刷；首次创建失败时传播 WinRT 错误且不缓存失败项。
    fn brush(&self, color: Color) -> windows_core::Result<SolidColorBrush> {
        let key = [color.A, color.R, color.G, color.B];
        let mut cache = self.brushes.borrow_mut();
        if let Some((_, brush)) = cache.iter().find(|(k, _)| *k == key) {
            return Ok(brush.clone());
        }
        let brush = solid(color)?;
        cache.push((key, brush.clone()));
        Ok(brush)
    }
}

impl CandidateTheme {
    /// 获取候选文字字体族并缓存成功结果；创建失败会在当前调用中向上传播。
    fn candidate_font(&self) -> windows_core::Result<FontFamily> {
        if let Some(font) = self.font.borrow().as_ref() {
            return Ok(font.clone());
        }
        let font = FontFamily::CreateInstanceWithName(&HSTRING::from(&self.config.font))?;
        *self.font.borrow_mut() = Some(font.clone());
        Ok(font)
    }

    /// 在宿主仍隐藏时准备共享视觉属性并验证必需字体与画刷。
    ///
    /// 此阶段不构建候选项、不注册事件，也不需要快照或事件发送端，可用于在
    /// 初始化期间提前发现 XAML 资源错误。系统配色只在首次准备时计算并缓存。
    /// 任一必需 WinRT 操作失败都会返回错误，供主题工厂中止初始化。
    pub fn prepare(
        &self,
        root: &Border,
        candidate_panel: &Grid,
        rows: &StackPanel,
        quick_action_panel: &Border,
        quick_actions: &StackPanel,
        mode_indicator: &ModeIndicatorVisual,
    ) -> windows_core::Result<()> {
        let palette = self.palette.get().unwrap_or_else(|| {
            let mut palette = Palette::system();
            palette.accent = self.config.accent(palette.dark, palette.accent);
            palette.foreground = self.config.foreground(palette.dark, palette.foreground);
            palette.background = self.config.background(palette.dark);
            palette
        });
        self.palette.set(Some(palette));
        self.brush(palette.foreground)?;
        self.brush(palette.accent)?;
        for alpha in [0xa0, 0x5c, 0x24, 0x0f, 0x20, 0x34, 0x70] {
            self.brush(with_alpha(palette.foreground, alpha))?;
        }
        self.brush(Color {
            A: 0,
            R: 0,
            G: 0,
            B: 0,
        })?;
        let panel_background = self.brush(palette.acrylic_base())?;
        let panel_border = self.brush(with_alpha(palette.foreground, 0x24))?;
        self.candidate_font()?;
        // Validate the icon font factory as well as the candidate font.
        FontFamily::CreateInstanceWithName(&HSTRING::from(icon_font()))?;

        root.SetPadding(Thickness {
            Left: 0.0,
            Top: 0.0,
            Right: 0.0,
            Bottom: 0.0,
        })?;
        root.SetMinWidth(140.0)?;
        root.SetCornerRadius(corner_radius(8.0))?;
        root.SetBorderThickness(thickness(1.0, 1.0, 1.0, 1.0))?;
        root.SetBorderBrush(&panel_border)?;
        root.SetRequestedTheme(if palette.dark {
            ElementTheme::Dark
        } else {
            ElementTheme::Light
        })?;
        apply_panel_background(root, &palette, &panel_background)?;

        candidate_panel.SetVisibility(Visibility::Visible)?;
        mode_indicator.panel.SetVisibility(Visibility::Collapsed)?;
        mode_indicator
            .label
            .SetFontFamily(&self.candidate_font()?)?;
        mode_indicator.label.SetFontSize(self.config.font_size)?;
        mode_indicator
            .label
            .SetTextAlignment(TextAlignment::Center)?;
        mode_indicator
            .label
            .SetHorizontalAlignment(HorizontalAlignment::Center)?;
        mode_indicator
            .label
            .SetVerticalAlignment(VerticalAlignment::Center)?;
        mode_indicator
            .label
            .SetForeground(&self.brush(palette.foreground)?)?;
        mode_indicator
            .accent
            .SetWidth((self.config.font_size * 1.15).max(16.0))?;
        mode_indicator.accent.SetHeight(3.0)?;
        mode_indicator
            .accent
            .SetMargin(thickness(0.0, 2.0, 0.0, 0.0))?;
        mode_indicator.accent.SetCornerRadius(corner_radius(1.5))?;
        mode_indicator
            .accent
            .SetHorizontalAlignment(HorizontalAlignment::Center)?;
        mode_indicator
            .accent
            .SetBackground(&self.brush(palette.accent)?)?;

        rows.SetOrientation(Orientation::Horizontal)?;
        rows.SetPadding(thickness(2.0, 2.0, 2.0, 2.0))?;
        rows.SetSpacing(0.0)?;

        quick_action_panel.SetBorderBrush(&panel_border)?;
        quick_action_panel.SetBorderThickness(thickness(1.0, 0.0, 0.0, 0.0))?;
        quick_action_panel.SetPadding(thickness(2.0, 2.0, 2.0, 2.0))?;
        quick_action_panel.SetVerticalAlignment(VerticalAlignment::Stretch)?;
        quick_actions.SetOrientation(Orientation::Horizontal)?;
        quick_actions.SetVerticalAlignment(VerticalAlignment::Center)?;
        Ok(())
    }

    /// 根据快照重建候选项与快捷操作，并将交互事件登记到调用方的撤销列表。
    ///
    /// 调用前应先撤销该列表中旧视觉树的回调；本方法清空两组子项，再逐项创建
    /// 控件。每个可用控件的回调克隆事件发送端，点击时发送对应 `UiAction`；
    /// 禁用项不注册交互回调。XAML 创建、属性设置或事件订阅失败会返回错误，
    /// 已追加的撤销句柄仍由调用方持有并负责清理。
    ///
    /// # 性能
    ///
    /// 每次调用都会重建候选和快捷操作子树；上层应仅在内容变化时调用。
    pub fn render(
        &self,
        root: &Border,
        candidate_panel: &Grid,
        rows: &StackPanel,
        quick_action_panel: &Border,
        quick_actions: &StackPanel,
        mode_indicator: &ModeIndicatorVisual,
        snapshot: &CandidateView,
        events: &crate::theme_api::EventSink,
        revokers: &mut Vec<windows_core::EventRevoker>,
    ) -> windows_core::Result<()> {
        self.prepare(
            root,
            candidate_panel,
            rows,
            quick_action_panel,
            quick_actions,
            mode_indicator,
        )?;
        let palette = self.palette.get().unwrap_or_else(Palette::system);
        self.palette.set(Some(palette));
        let solid = |color| self.brush(color);
        let foreground = solid(palette.foreground)?;
        let secondary_foreground = solid(with_alpha(palette.foreground, 0xa0))?;
        let disabled_foreground = solid(with_alpha(palette.foreground, 0x5c))?;
        let accent = solid(palette.accent)?;
        let panel_border = solid(with_alpha(palette.foreground, 0x24))?;
        let selection_background = solid(with_alpha(palette.foreground, 0x0f))?;
        let hover_background = solid(with_alpha(palette.foreground, 0x20))?;
        let pressed_background = solid(with_alpha(palette.foreground, 0x34))?;
        let pressed_foreground = solid(with_alpha(palette.foreground, 0x70))?;
        let transparent = solid(Color {
            A: 0,
            R: 0,
            G: 0,
            B: 0,
        })?;

        let children = rows.Children()?;
        children.Clear()?;
        let action_children = quick_actions.Children()?;
        action_children.Clear()?;
        if let Some(indicator) = &snapshot.mode_indicator {
            let side = self.config.font_size * 1.8 + 10.0;
            root.SetMinWidth(0.0)?;
            root.SetWidth(side)?;
            root.SetHeight(side)?;
            candidate_panel.SetVisibility(Visibility::Collapsed)?;
            mode_indicator.panel.SetVisibility(Visibility::Visible)?;
            mode_indicator
                .label
                .SetText(&HSTRING::from(if indicator.ascii_mode {
                    "英"
                } else {
                    "中"
                }))?;
            return Ok(());
        }
        root.SetWidth(f64::NAN)?;
        root.SetHeight(f64::NAN)?;
        root.SetMinWidth(140.0)?;
        candidate_panel.SetVisibility(Visibility::Visible)?;
        mode_indicator.panel.SetVisibility(Visibility::Collapsed)?;
        quick_action_panel.SetVisibility(Visibility::Visible)?;
        append_action(
            quick_actions,
            "\u{EDD9}",
            8.0,
            thickness(2.0, 2.0, 3.0, 2.0),
            snapshot.can_page_previous,
            UiAction::NavigatePrevious,
            events,
            &foreground,
            &disabled_foreground,
            &hover_background,
            &pressed_background,
            &pressed_foreground,
            revokers,
        )?;
        append_action(
            quick_actions,
            "\u{EDDA}",
            8.0,
            thickness(1.0, 2.0, 2.0, 2.0),
            snapshot.can_page_next,
            UiAction::NavigateNext,
            events,
            &foreground,
            &disabled_foreground,
            &hover_background,
            &pressed_background,
            &pressed_foreground,
            revokers,
        )?;
        let divider = Border::new()?;
        divider.SetWidth(1.0)?;
        divider.SetMargin(thickness(2.0, -2.0, 2.0, -2.0))?;
        divider.SetVerticalAlignment(VerticalAlignment::Stretch)?;
        divider.SetBackground(&panel_border)?;
        action_children.Append(&divider)?;
        append_action(
            quick_actions,
            emoji_glyph_for_version(OsVersion::current()),
            16.0,
            thickness(2.0, 2.0, 2.0, 2.0),
            true,
            UiAction::OpenEmojiPanel,
            events,
            &foreground,
            &disabled_foreground,
            &hover_background,
            &pressed_background,
            &pressed_foreground,
            revokers,
        )?;

        let candidate_font = self.candidate_font()?;
        for (index, candidate) in snapshot.items.iter().enumerate() {
            let item_index = index as u32;
            let selected = item_index == snapshot.selected_index;
            let row = Border::new()?;
            row.SetMinWidth(62.0)?;
            row.SetMinHeight(24.0)?;
            row.SetMargin(thickness(2.0, 2.0, 2.0, 2.0))?;
            row.SetPadding(thickness(6.0, 0.0, 6.0, 0.0))?;
            row.SetCornerRadius(corner_radius(2.0))?;
            row.SetBackground(if selected {
                &selection_background
            } else {
                &transparent
            })?;

            let line = StackPanel::new()?;
            line.SetOrientation(Orientation::Horizontal)?;
            line.SetVerticalAlignment(VerticalAlignment::Center)?;
            line.SetSpacing(0.0)?;
            let line_children = line.Children()?;

            let indicator = Border::new()?;
            indicator.SetWidth(3.0)?;
            indicator.SetHeight(16.0)?;
            indicator.SetMargin(thickness(0.0, 0.0, 6.0, 0.0))?;
            indicator.SetCornerRadius(corner_radius(2.0))?;
            indicator.SetVerticalAlignment(VerticalAlignment::Center)?;
            indicator.SetBackground(if selected { &accent } else { &transparent })?;

            let ordinal = TextBlock::new()?;
            ordinal.SetFontFamily(&candidate_font)?;
            ordinal.SetFontSize(self.config.font_size)?;
            ordinal.SetVerticalAlignment(VerticalAlignment::Center)?;
            ordinal.SetText(&HSTRING::from((index + 1).to_string()))?;
            ordinal.SetMargin(thickness(0.0, 0.0, 6.0, 0.0))?;
            ordinal.SetForeground(&secondary_foreground)?;

            let text = TextBlock::new()?;
            text.SetFontFamily(&candidate_font)?;
            text.SetFontSize(self.config.font_size)?;
            text.SetVerticalAlignment(VerticalAlignment::Center)?;
            let label = if candidate.secondary_text.is_empty() {
                candidate.primary_text.clone()
            } else {
                format!("{}  {}", candidate.primary_text, candidate.secondary_text)
            };
            text.SetText(&HSTRING::from(label))?;
            text.SetMargin(thickness(0.0, 0.0, 8.0, 0.0))?;
            text.SetForeground(&foreground)?;

            line_children.Append(&indicator)?;
            line_children.Append(&ordinal)?;
            line_children.Append(&text)?;
            row.SetChild(&line)?;

            let hover_row = row.clone();
            let hover_brush = hover_background.clone();
            revokers.push(row.PointerEntered(move |_, _| {
                let _ = hover_row.SetBackground(&hover_brush);
            })?);
            let restore_row = row.clone();
            let restore_brush = if selected {
                selection_background.clone()
            } else {
                transparent.clone()
            };
            revokers.push(row.PointerExited(move |_, _| {
                let _ = restore_row.SetBackground(&restore_brush);
            })?);
            if candidate.enabled {
                let exited_text = text.clone();
                let exited_ordinal = ordinal.clone();
                let exited_indicator = indicator.clone();
                let exited_text_brush = foreground.clone();
                let exited_ordinal_brush = secondary_foreground.clone();
                revokers.push(row.PointerExited(move |_, _| {
                    let _ = exited_text.SetForeground(&exited_text_brush);
                    let _ = exited_ordinal.SetForeground(&exited_ordinal_brush);
                    let _ = exited_indicator.SetHeight(16.0);
                })?);
                let pressed_row = row.clone();
                let pressed_text = text.clone();
                let pressed_ordinal = ordinal.clone();
                let pressed_indicator = indicator.clone();
                let pressed_brush = pressed_background.clone();
                let pressed_text_brush = pressed_foreground.clone();
                revokers.push(row.PointerPressed(move |_, _| {
                    let _ = pressed_row.SetBackground(&pressed_brush);
                    let _ = pressed_text.SetForeground(&pressed_text_brush);
                    let _ = pressed_ordinal.SetForeground(&pressed_text_brush);
                    let _ = pressed_indicator.SetHeight(10.0);
                })?);
                let released_row = row.clone();
                let released_text = text.clone();
                let released_ordinal = ordinal.clone();
                let released_indicator = indicator.clone();
                let released_row_brush = if selected {
                    selection_background.clone()
                } else {
                    transparent.clone()
                };
                let released_ordinal_brush = secondary_foreground.clone();
                let released_text_brush = foreground.clone();
                revokers.push(row.PointerReleased(move |_, _| {
                    let _ = released_row.SetBackground(&released_row_brush);
                    let _ = released_text.SetForeground(&released_text_brush);
                    let _ = released_ordinal.SetForeground(&released_ordinal_brush);
                    let _ = released_indicator.SetHeight(16.0);
                })?);
                let event_sender = events.clone();
                revokers.push(row.Tapped(move |_, _| {
                    event_sender.send(UiAction::ItemInvoked(item_index));
                })?);
            }
            children.Append(&row)?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
/// 创建一个快捷操作控件，并仅在启用时注册悬停、按压、释放和点击回调。
///
/// 事件撤销句柄追加到调用方提供的列表，保证其生命周期覆盖该控件的可见期；
/// 点击事件通过克隆的发送端发出。禁用时只显示禁用态字形。任何 XAML 操作
/// 失败均返回错误，已登记的句柄由调用方统一撤销。
fn append_action(
    actions: &StackPanel,
    label: &str,
    font_size: f64,
    margin: Thickness,
    enabled: bool,
    action: UiAction,
    events: &crate::theme_api::EventSink,
    foreground: &SolidColorBrush,
    disabled_foreground: &SolidColorBrush,
    hover_background: &SolidColorBrush,
    pressed_background: &SolidColorBrush,
    pressed_foreground: &SolidColorBrush,
    revokers: &mut Vec<windows_core::EventRevoker>,
) -> windows_core::Result<()> {
    let item = Border::new()?;
    item.SetMinWidth(24.0)?;
    item.SetMinHeight(24.0)?;
    item.SetMargin(margin)?;
    item.SetCornerRadius(corner_radius(2.0))?;
    let transparent = SolidColorBrush::CreateInstanceWithColor(Color {
        A: 0,
        R: 0,
        G: 0,
        B: 0,
    })?;
    item.SetBackground(&transparent)?;
    let text = TextBlock::new()?;
    // Glyphs and font must be selected together for older Windows 10 fonts.
    text.SetFontFamily(&FontFamily::CreateInstanceWithName(&HSTRING::from(
        icon_font(),
    ))?)?;
    text.SetText(&HSTRING::from(label))?;
    text.SetFontSize(font_size)?;
    text.SetHorizontalAlignment(HorizontalAlignment::Center)?;
    text.SetVerticalAlignment(VerticalAlignment::Center)?;
    text.SetForeground(if enabled {
        foreground
    } else {
        disabled_foreground
    })?;
    item.SetChild(&text)?;
    if enabled {
        let hover_item = item.clone();
        let hover_brush = hover_background.clone();
        revokers.push(item.PointerEntered(move |_, _| {
            let _ = hover_item.SetBackground(&hover_brush);
        })?);
        let leave_item = item.clone();
        let leave_text = text.clone();
        let leave_foreground = foreground.clone();
        let leave_background = transparent.clone();
        revokers.push(item.PointerExited(move |_, _| {
            let _ = leave_item.SetBackground(&leave_background);
            let _ = leave_text.SetForeground(&leave_foreground);
        })?);
        let pressed_item = item.clone();
        let pressed_text = text.clone();
        let pressed_background = pressed_background.clone();
        let pressed_foreground = pressed_foreground.clone();
        revokers.push(item.PointerPressed(move |_, _| {
            let _ = pressed_item.SetBackground(&pressed_background);
            let _ = pressed_text.SetForeground(&pressed_foreground);
        })?);
        let released_item = item.clone();
        let released_text = text.clone();
        let released_foreground = foreground.clone();
        let released_background = transparent.clone();
        revokers.push(item.PointerReleased(move |_, _| {
            let _ = released_item.SetBackground(&released_background);
            let _ = released_text.SetForeground(&released_foreground);
        })?);
        let event_sender = events.clone();
        revokers.push(item.Tapped(move |_, _| {
            event_sender.send(action);
        })?);
    }
    actions.Children()?.Append(&item)
}

#[derive(Clone, Copy)]
/// 单次渲染使用的系统与用户配色快照。
struct Palette {
    /// 用户显式设置的背景色；没有时由明暗模式生成亚克力基色。
    background: Option<Color>,
    /// 系统或用户指定的前景文字色。
    foreground: Color,
    /// 系统或用户指定的强调色。
    accent: Color,
    /// 由系统背景亮度推导的深色模式标记。
    dark: bool,
}

impl Palette {
    /// 读取系统背景、文字和强调色；读取不完整时使用内置浅色回退方案。
    fn system() -> Self {
        let fallback = Self {
            background: None,
            foreground: Color {
                A: 0xff,
                R: 0x20,
                G: 0x20,
                B: 0x20,
            },
            accent: Color {
                A: 0xff,
                R: 0x00,
                G: 0x78,
                B: 0xd4,
            },
            dark: false,
        };
        let Ok(settings) = UISettings::new() else {
            return fallback;
        };
        let (Ok(background), Ok(foreground), Ok(accent)) = (
            settings.GetColorValue(UIColorType::Background),
            settings.GetColorValue(UIColorType::Foreground),
            settings.GetColorValue(UIColorType::Accent),
        ) else {
            return fallback;
        };
        let luminance = 299_u32 * background.R as u32
            + 587_u32 * background.G as u32
            + 114_u32 * background.B as u32;
        Self {
            background: None,
            foreground,
            accent,
            dark: luminance < 128_000,
        }
    }

    /// 返回显式背景色或按明暗模式选取的亚克力不透明回退色。
    fn acrylic_base(self) -> Color {
        if let Some(background) = self.background {
            return background;
        }
        Color {
            A: 0xff,
            R: if self.dark { 0x1c } else { 0xf3 },
            G: if self.dark { 0x1c } else { 0xf3 },
            B: if self.dark { 0x1c } else { 0xf3 },
        }
    }
}

/// 尝试为根面板配置 HostBackdrop 亚克力；创建或配置失败时记录诊断并设为纯色。
///
/// 亚克力不可用本身不视为主题错误，只要纯色回退背景成功设置就返回成功；
/// 回退背景设置失败则将 WinRT 错误传回调用方。
fn apply_panel_background(
    root: &Border,
    palette: &Palette,
    fallback: &SolidColorBrush,
) -> windows_core::Result<()> {
    let acrylic = match AcrylicBrush::new() {
        Ok(acrylic) => acrylic,
        Err(error) => {
            crate::diagnostics::record(format_args!(
                "AcrylicBrush creation failed; using solid fallback: {error}"
            ));
            return root.SetBackground(fallback);
        }
    };

    let result = (|| -> windows_core::Result<()> {
        acrylic.SetBackgroundSource(AcrylicBackgroundSource::HostBackdrop)?;
        acrylic.SetFallbackColor(palette.acrylic_base())?;
        acrylic.SetTintColor(palette.acrylic_base())?;
        acrylic.SetTintOpacity(
            palette
                .background
                .map_or(if palette.dark { 0.75 } else { 0.0 }, |c| {
                    c.A as f64 / 255.0
                }),
        )?;
        acrylic.SetTintLuminosityOpacity(Some(if palette.dark { 0.92 } else { 0.9 }))?;
        root.SetBackground(&acrylic)
    })();

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            crate::diagnostics::record(format_args!(
                "acrylic configuration failed; using solid fallback: {error}"
            ));
            root.SetBackground(fallback)
        }
    }
}

/// 创建未缓存的纯色 WinRT 画刷，并保留底层资源错误。
fn solid(color: Color) -> windows_core::Result<SolidColorBrush> {
    SolidColorBrush::CreateInstanceWithColor(color)
}

/// 保留 RGB 通道并替换颜色的不透明度通道。
fn with_alpha(mut color: Color, alpha: u8) -> Color {
    color.A = alpha;
    color
}

/// 按左、上、右、下顺序构造 XAML 边距/边框厚度值。
fn thickness(left: f64, top: f64, right: f64, bottom: f64) -> Thickness {
    Thickness {
        Left: left,
        Top: top,
        Right: right,
        Bottom: bottom,
    }
}

/// 为四个角设置相同半径。
fn corner_radius(value: f64) -> CornerRadius {
    CornerRadius {
        TopLeft: value,
        TopRight: value,
        BottomRight: value,
        BottomLeft: value,
    }
}
