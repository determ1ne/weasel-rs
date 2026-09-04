use weasel_common::message::{RenderSnapshot, RendererEvent, RendererEventAction};
use windows_core::HSTRING;
use windows_version::OsVersion;

fn icon_font_for_version(version: OsVersion) -> &'static str {
    const WINDOWS_10: OsVersion = OsVersion::new(10, 0, 0, 0);
    const WINDOWS_11: OsVersion = OsVersion::new(10, 0, 0, 22_000);
    if version >= WINDOWS_10 && version < WINDOWS_11 {
        "Segoe MDL2 Assets"
    } else {
        "Segoe Fluent Icons"
    }
}

fn icon_font() -> &'static str {
    static FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FONT.get_or_init(|| icon_font_for_version(OsVersion::current()))
}

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
    HorizontalAlignment, Orientation, SolidColorBrush, StackPanel, TextBlock, Thickness,
    UIColorType, UISettings, VerticalAlignment,
};

/// Visual construction is a theme concern; the RPC protocol remains generic.
pub trait RenderTheme {
    fn render(
        &self,
        root: &Border,
        rows: &StackPanel,
        quick_action_panel: &Border,
        quick_actions: &StackPanel,
        snapshot: &RenderSnapshot,
        events: &crate::xaml_host::EventSender,
        revokers: &mut Vec<windows_core::EventRevoker>,
    ) -> windows_core::Result<()>;
}

#[derive(Default)]
pub struct CandidateTheme {
    brushes: std::cell::RefCell<Vec<([u8; 4], SolidColorBrush)>>,
    palette: std::cell::Cell<Option<Palette>>,
    font: std::cell::RefCell<Option<FontFamily>>,
}

impl CandidateTheme {
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

impl RenderTheme for CandidateTheme {
    fn render(
        &self,
        root: &Border,
        rows: &StackPanel,
        quick_action_panel: &Border,
        quick_actions: &StackPanel,
        snapshot: &RenderSnapshot,
        events: &crate::xaml_host::EventSender,
        revokers: &mut Vec<windows_core::EventRevoker>,
    ) -> windows_core::Result<()> {
        let palette = self.palette.get().unwrap_or_else(Palette::system);
        self.palette.set(Some(palette));
        let solid = |color| self.brush(color);
        let foreground = solid(palette.foreground)?;
        let secondary_foreground = solid(with_alpha(palette.foreground, 0xa0))?;
        let disabled_foreground = solid(with_alpha(palette.foreground, 0x5c))?;
        let accent = solid(palette.accent)?;
        let panel_background = solid(palette.acrylic_base())?;
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

        rows.SetOrientation(Orientation::Horizontal)?;
        rows.SetPadding(thickness(2.0, 2.0, 2.0, 2.0))?;
        rows.SetSpacing(0.0)?;
        let children = rows.Children()?;
        children.Clear()?;

        quick_action_panel.SetBorderBrush(&panel_border)?;
        quick_action_panel.SetBorderThickness(thickness(1.0, 0.0, 0.0, 0.0))?;
        quick_action_panel.SetPadding(thickness(2.0, 2.0, 2.0, 2.0))?;
        quick_action_panel.SetVerticalAlignment(VerticalAlignment::Stretch)?;
        quick_actions.SetOrientation(Orientation::Horizontal)?;
        quick_actions.SetVerticalAlignment(VerticalAlignment::Center)?;
        let action_children = quick_actions.Children()?;
        action_children.Clear()?;
        append_action(
            quick_actions,
            "\u{EDD9}",
            8.0,
            thickness(2.0, 2.0, 3.0, 2.0),
            snapshot.can_page_previous,
            RendererEventAction::NavigatePrevious,
            snapshot,
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
            RendererEventAction::NavigateNext,
            snapshot,
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
            RendererEventAction::OpenEmojiPanel,
            snapshot,
            events,
            &foreground,
            &disabled_foreground,
            &hover_background,
            &pressed_background,
            &pressed_foreground,
            revokers,
        )?;

        let candidate_font = match self.font.borrow().as_ref() {
            Some(font) => font.clone(),
            None => FontFamily::CreateInstanceWithName(&HSTRING::from("Microsoft YaHei UI"))?,
        };
        *self.font.borrow_mut() = Some(candidate_font.clone());
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
            ordinal.SetFontSize(14.0)?;
            ordinal.SetVerticalAlignment(VerticalAlignment::Center)?;
            ordinal.SetText(&HSTRING::from((index + 1).to_string()))?;
            ordinal.SetMargin(thickness(0.0, 0.0, 6.0, 0.0))?;
            ordinal.SetForeground(&secondary_foreground)?;

            let text = TextBlock::new()?;
            text.SetFontFamily(&candidate_font)?;
            text.SetFontSize(14.0)?;
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
                let session_id = snapshot.session_id;
                let token = snapshot.token.clone();
                let revision = snapshot.revision;
                revokers.push(row.Tapped(move |_, _| {
                    let _ = event_sender.send(RendererEvent {
                        session_id,
                        action: RendererEventAction::ItemInvoked as i32,
                        item_index,
                        token: token.clone(),
                        revision,
                    });
                })?);
            }
            children.Append(&row)?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn append_action(
    actions: &StackPanel,
    label: &str,
    font_size: f64,
    margin: Thickness,
    enabled: bool,
    action: RendererEventAction,
    snapshot: &RenderSnapshot,
    events: &crate::xaml_host::EventSender,
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
        let session_id = snapshot.session_id;
        let token = snapshot.token.clone();
        let revision = snapshot.revision;
        revokers.push(item.Tapped(move |_, _| {
            let _ = event_sender.send(RendererEvent {
                session_id,
                action: action as i32,
                item_index: 0,
                token: token.clone(),
                revision,
            });
        })?);
    }
    actions.Children()?.Append(&item)
}

#[derive(Clone, Copy)]
struct Palette {
    foreground: Color,
    accent: Color,
    dark: bool,
}

impl Palette {
    fn system() -> Self {
        let fallback = Self {
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
            foreground,
            accent,
            dark: luminance < 128_000,
        }
    }

    fn acrylic_base(self) -> Color {
        Color {
            A: 0xff,
            R: if self.dark { 0x1c } else { 0xf3 },
            G: if self.dark { 0x1c } else { 0xf3 },
            B: if self.dark { 0x1c } else { 0xf3 },
        }
    }
}

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
        acrylic.SetTintOpacity(if palette.dark { 0.75 } else { 0.0 })?;
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

fn solid(color: Color) -> windows_core::Result<SolidColorBrush> {
    SolidColorBrush::CreateInstanceWithColor(color)
}

fn with_alpha(mut color: Color, alpha: u8) -> Color {
    color.A = alpha;
    color
}

fn thickness(left: f64, top: f64, right: f64, bottom: f64) -> Thickness {
    Thickness {
        Left: left,
        Top: top,
        Right: right,
        Bottom: bottom,
    }
}

fn corner_radius(value: f64) -> CornerRadius {
    CornerRadius {
        TopLeft: value,
        TopRight: value,
        BottomRight: value,
        BottomLeft: value,
    }
}
