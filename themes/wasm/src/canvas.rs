//! Direct2D/DirectWrite 封装：WASM 主题后端中唯一接触 D2D/DWrite 绑定的文件。
//!
//! WASM 主题通过 host 导入函数产出 [`protocol::DrawCommand`] 命令流；本文件负责
//! 把命令流转换为真实 D2D 渲染，并集中管理：
//! - D2D 工厂与 DirectWrite 工厂的创建
//! - 字体槽位（[`protocol::FONT_TEXT`] 等）× 字号 的文本格式按需缓存
//! - Composition 透明表面生命周期
//! - 每帧命令回放（`BeginDraw`/`EndDraw`，RAII 保证 `EndDraw` 必达）
//! - 文本测量（DirectWrite `GetMetrics`）
//!
//! Pixels use premultiplied alpha; an untouched surface is transparent.
//!
//! 本模块不创建窗口；HWND、DPI、设备丢失重试策略属于 window.rs（步骤 3）。
//! 所有方法只能在创建它的 UI 线程上调用（D2D 单线程工厂）。

use crate::d2d_bindings::*;
use crate::protocol::{
    DrawCommand, FONT_COMMENT, FONT_ICON, FONT_NUMBER, FONT_TEXT, FONT_TEXT_BOLD,
};
use std::collections::HashMap;
use windows_core::{Error, Result as WinResult};
use windows_strings::{PCWSTR, w};

/// 文本布局矩形的远端边界余量：文本按 `(x, y)` 左上锚点绘制，
/// 矩形右/下边界取一个远超窗口尺寸的值以避免 `CLIP` 裁剪。
const TEXT_EXTENT: f32 = 65_536.0;

/// 字号合法范围（DIP）：小于 4 时 DirectWrite 输出退化，大于 512 无实际意义。
const SIZE_MIN: f32 = 4.0;
const SIZE_MAX: f32 = 512.0;

/// D2D/DWrite 渲染器。COM 接口不 `Send`，实例必须留在创建线程上。
pub struct Canvas {
    write: IDWriteFactory,
    /// (字体槽位, 字号位模式) → 文本格式；DirectWrite 格式是轻量 CPU 对象，
    /// 按值缓存。字号用 `to_bits()` 做键，保证 f32 可哈希且同值同键。
    formats: HashMap<(i32, u32), IDWriteTextFormat>,
    families: HashMap<i32, windows_strings::HSTRING>,
    presenter: Option<crate::composition::Presenter>,
    panel: crate::protocol::PanelStyle,
    content_size: (f32, f32),
    dpi: u32,
    last_frame: Option<Vec<DrawCommand>>,
}

/// 字体槽位 → (字体族, 对齐方式)，映射规则与 ten 主题一致。
/// 未知槽位回退为正文格式，保证坏主题仍可见。
fn font_slot(font: i32) -> (PCWSTR, DWRITE_TEXT_ALIGNMENT) {
    match font {
        FONT_NUMBER => (w!("Segoe UI"), DWRITE_TEXT_ALIGNMENT_LEADING),
        FONT_COMMENT => (w!("Microsoft YaHei UI"), DWRITE_TEXT_ALIGNMENT_LEADING),
        FONT_ICON => (w!("Segoe MDL2 Assets"), DWRITE_TEXT_ALIGNMENT_LEADING),
        _ => (w!("Microsoft YaHei UI"), DWRITE_TEXT_ALIGNMENT_LEADING),
    }
}

/// Straight ARGB colors; D2D writes premultiplied pixels into the composition surface.
fn to_color(rgb: u32) -> D2D_COLOR_F {
    D2D_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: ((rgb >> 24) & 0xff) as f32 / 255.0,
    }
}

/// `StrokeRect` 的四条边（上/下/左/右）；宽度被限制在半宽/半高内，
/// 非法尺寸（w、h、width ≤ 0）退化为零矩形。
fn stroke_edges(x: f32, y: f32, w: f32, h: f32, width: f32) -> [D2D_RECT_F; 4] {
    if w <= 0.0 || h <= 0.0 || width <= 0.0 {
        return [D2D_RECT_F::default(); 4];
    }
    let width = width.min(w / 2.0).min(h / 2.0);
    [
        D2D_RECT_F {
            left: x,
            top: y,
            right: x + w,
            bottom: y + width,
        },
        D2D_RECT_F {
            left: x,
            top: y + h - width,
            right: x + w,
            bottom: y + h,
        },
        D2D_RECT_F {
            left: x,
            top: y,
            right: x + width,
            bottom: y + h,
        },
        D2D_RECT_F {
            left: x + w - width,
            top: y,
            right: x + w,
            bottom: y + h,
        },
    ]
}

impl Canvas {
    pub fn new() -> WinResult<Self> {
        unsafe {
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self {
                write,
                formats: HashMap::new(),
                families: HashMap::new(),
                presenter: None,
                panel: Default::default(),
                content_size: (1.0, 1.0),
                dpi: 96,
                last_frame: None,
            })
        }
    }

    pub fn ensure_target(&mut self, hwnd: HWND, dpi: u32) -> WinResult<()> {
        if self.presenter.is_none() {
            let mut rect = RECT::default();
            unsafe {
                GetClientRect(hwnd, &mut rect).ok()?;
            }
            self.presenter = Some(crate::composition::Presenter::new(
                hwnd,
                dpi,
                (rect.right - rect.left).max(1) as u32,
                (rect.bottom - rect.top).max(1) as u32,
            )?);
        }
        self.dpi = dpi;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> WinResult<()> {
        if self.dpi != dpi {
            self.last_frame = None;
        }
        self.dpi = dpi;
        if let Some(presenter) = &mut self.presenter {
            presenter.resize(width, height, dpi)?;
        }
        Ok(())
    }

    pub fn set_panel(
        &mut self,
        style: crate::protocol::PanelStyle,
        size: (f32, f32),
        dpi: u32,
    ) -> WinResult<()> {
        if self.panel != style || self.content_size != size || self.dpi != dpi {
            self.last_frame = None;
        }
        self.panel = style;
        self.content_size = size;
        self.dpi = dpi;
        if let Some(presenter) = &mut self.presenter {
            presenter.set_panel(&style, size.0.max(1.0), size.1.max(1.0), dpi)?;
        }
        Ok(())
    }

    pub fn invalidate_target(&mut self) {
        self.presenter = None;
        self.last_frame = None;
    }

    pub fn set_backdrop(&mut self, style: crate::protocol::BackdropStyle) -> WinResult<()> {
        if let Some(presenter) = &mut self.presenter {
            presenter.set_backdrop(style)?;
        }
        Ok(())
    }

    /// 错误是否为 D2D 设备丢失（决定是否进入重试路径而非直接失败）。
    pub fn is_device_lost(error: &Error) -> bool {
        matches!(
            error.code(),
            D2DERR_RECREATE_TARGET | DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET
        )
    }

    /// 取 (槽位, 字号) 对应的 DirectWrite 文本格式，缺省时创建并缓存。
    fn get_format(&mut self, font: i32, size: f32) -> WinResult<IDWriteTextFormat> {
        let size = size.clamp(SIZE_MIN, SIZE_MAX);
        let key = (font, size.to_bits());
        if let Some(format) = self.formats.get(&key) {
            return Ok(format.clone());
        }
        unsafe {
            let (family, align) = font_slot(font);
            let format = self.write.CreateTextFormat(
                self.families
                    .get(
                        &(if font == FONT_TEXT_BOLD {
                            FONT_TEXT
                        } else {
                            font
                        }),
                    )
                    .map(|s| s.as_ptr())
                    .map(PCWSTR)
                    .unwrap_or(family),
                None,
                if font == FONT_TEXT_BOLD {
                    DWRITE_FONT_WEIGHT_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("zh-CN"),
            )?;
            format.SetTextAlignment(align).ok()?;
            format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)
                .ok()?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
            // Guest-selected sizes must not grow a process-lifetime cache forever.
            if self.formats.len() >= 128 {
                self.formats.clear();
            }
            self.formats.insert(key, format.clone());
            Ok(format)
        }
    }

    pub fn set_font(&mut self, slot: i32, family: &str) {
        self.last_frame = None;
        self.families
            .insert(slot, windows_strings::HSTRING::from(family));
        self.formats.retain(|(font, _), _| {
            *font != slot && !(slot == FONT_TEXT && *font == FONT_TEXT_BOLD)
        });
    }

    pub fn line_height(&mut self, font: i32, size: f32) -> WinResult<f32> {
        let format = self.get_format(font, size)?;
        unsafe {
            let layout =
                self.write
                    .CreateTextLayout(&[0x004d, 0x4e2d], &format, 10000.0, 4096.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).ok()?;
            Ok(metrics.height)
        }
    }

    /// 文本宽度（DIP，含尾随空格），与 `draw_text` 使用同一格式，
    /// 保证主题测量与实绘一致。
    pub fn measure(&mut self, font: i32, size: f32, text: &str) -> WinResult<f32> {
        let format = self.get_format(font, size)?;
        let text: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout =
                self.write
                    .CreateTextLayout(&text, &format, 1_000_000.0, size.max(16.0) * 4.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).ok()?;
            Ok(metrics.widthIncludingTrailingWhitespace)
        }
    }

    /// 回放一帧命令：`BeginDraw` → 清屏 → 逐条绘制 → `EndDraw`。
    /// Transparent surface; unchanged command lists reuse the compositor's retained content.
    pub fn replay(&mut self, commands: &[DrawCommand]) -> WinResult<()> {
        if self.last_frame.as_deref() == Some(commands) {
            return Ok(());
        }
        let target = self
            .presenter
            .as_mut()
            .ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?
            .begin_draw()?;
        let result: WinResult<()> = (|| unsafe {
            // BeginDraw 之前分配好本帧全部 D2D 资源（与 ten 主题一致）。
            let mut brushes: Vec<(u32, ID2D1SolidColorBrush)> = Vec::new();
            let mut formats: Vec<(usize, IDWriteTextFormat)> = Vec::new();
            for (index, command) in commands.iter().enumerate() {
                let color = match command {
                    DrawCommand::FillRect { color, .. }
                    | DrawCommand::FillRoundedRect { color, .. }
                    | DrawCommand::StrokeRect { color, .. }
                    | DrawCommand::Text { color, .. } => *color,
                };
                if !brushes.iter().any(|(c, _)| *c == color) {
                    brushes.push((color, target.CreateSolidColorBrush(&to_color(color), None)?));
                }
                if let DrawCommand::Text { font, size, .. } = command {
                    formats.push((index, self.get_format(*font, *size)?));
                }
            }
            let brush = |color: u32| -> WinResult<&ID2D1SolidColorBrush> {
                Ok(&brushes
                    .iter()
                    .find(|(c, _)| *c == color)
                    .expect("本帧已收集的颜色必然有画刷")
                    .1)
            };

            target.Clear(Some(&D2D_COLOR_F::default()));
            for (index, command) in commands.iter().enumerate() {
                // 非有限坐标的命令（NaN/Inf）跳过：D2D 对此行为未定义。
                if !command.is_finite() {
                    continue;
                }
                let color = match command {
                    DrawCommand::FillRect { color, .. }
                    | DrawCommand::FillRoundedRect { color, .. }
                    | DrawCommand::StrokeRect { color, .. }
                    | DrawCommand::Text { color, .. } => *color,
                };
                let brush = brush(color)?;
                match command {
                    DrawCommand::FillRoundedRect {
                        x, y, w, h, radius, ..
                    } => {
                        if *w <= 0.0 || *h <= 0.0 {
                            continue;
                        }
                        let radius = radius.max(0.0).min(w.min(*h) / 2.0);
                        target.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: *x,
                                    top: *y,
                                    right: x + w,
                                    bottom: y + h,
                                },
                                radiusX: radius,
                                radiusY: radius,
                            },
                            brush,
                        );
                    }
                    DrawCommand::FillRect { x, y, w, h, .. } => {
                        let rect = D2D_RECT_F {
                            left: *x,
                            top: *y,
                            right: x + w,
                            bottom: y + h,
                        };
                        target.FillRectangle(&rect, brush);
                    }
                    DrawCommand::StrokeRect {
                        x, y, w, h, width, ..
                    } => {
                        for edge in stroke_edges(*x, *y, *w, *h, *width) {
                            target.FillRectangle(&edge, brush);
                        }
                    }
                    DrawCommand::Text {
                        x,
                        y,
                        size,
                        text,
                        glow,
                        ..
                    } => {
                        let format = &formats
                            .iter()
                            .find(|(i, _)| *i == index)
                            .expect("文本命令的格式已预取")
                            .1;
                        let text: Vec<u16> = text.encode_utf16().collect();
                        if glow.0 > 0.0 && glow.1 >> 24 != 0 && !text.is_empty() {
                            // Bounded two-ring soft halo, sharing one shaped layout with
                            // the foreground. No extra guest calls or retained textures.
                            let layout = self.write.CreateTextLayout(
                                &text,
                                format,
                                TEXT_EXTENT,
                                size.clamp(SIZE_MIN, SIZE_MAX) * 2.0,
                            )?;
                            let halo = target.CreateSolidColorBrush(&to_color(glow.1), None)?;
                            for (radius, strength) in [(glow.0, 0.08), (glow.0 * 0.5, 0.16)] {
                                let mut color = to_color(glow.1);
                                color.a *= strength;
                                halo.SetColor(&color);
                                for (dx, dy) in [
                                    (1., 0.),
                                    (-1., 0.),
                                    (0., 1.),
                                    (0., -1.),
                                    (0.707, 0.707),
                                    (-0.707, 0.707),
                                    (0.707, -0.707),
                                    (-0.707, -0.707),
                                ] {
                                    target.DrawTextLayout(
                                        windows_numerics::Vector2 {
                                            x: x + dx * radius,
                                            y: y + dy * radius,
                                        },
                                        &layout,
                                        &halo,
                                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                                    );
                                }
                            }
                            target.DrawTextLayout(
                                windows_numerics::Vector2 { x: *x, y: *y },
                                &layout,
                                brush,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                            continue;
                        }
                        let rect = D2D_RECT_F {
                            left: *x,
                            top: *y,
                            right: x + TEXT_EXTENT,
                            bottom: y + size.clamp(SIZE_MIN, SIZE_MAX) * 2.0,
                        };
                        target.DrawText(
                            &text,
                            format,
                            &rect,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
            }
            Ok(())
        })();
        let present = self
            .presenter
            .as_mut()
            .ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?
            .end_draw();
        result?;
        present?;
        self.last_frame = Some(commands.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Windows::Win32::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
    use crate::protocol::FONT_TEXT;

    const CLASS: PCWSTR = w!("Weasel.ThemeWasm.CanvasTest");

    /// 测试窗口的默认过程：透传给 `DefWindowProcW`（需 `extern "system"` ABI）。
    unsafe extern "system" fn test_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    /// 真实 D2D 路径：隐藏窗口上的目标创建、测量、回放、失效重建。
    #[test]
    fn canvas_measures_and_replays_on_hidden_window() {
        unsafe {
            assert!(
                RoInitialize(RO_INIT_SINGLETHREADED).ok().is_ok(),
                "COM apartment 初始化失败"
            );
            let instance = GetModuleHandleW(None);
            assert!(!instance.0.is_null());
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(test_wnd_proc),
                ..Default::default()
            };
            assert!(RegisterClassW(&wc).0 != 0, "窗口类注册失败");
            let hwnd = CreateWindowExW(
                0,
                CLASS,
                None,
                WS_POPUP,
                0,
                0,
                300,
                60,
                None,
                None,
                Some(instance),
                None,
            );
            assert!(!hwnd.0.is_null(), "窗口创建失败");

            let mut canvas = Canvas::new().expect("Canvas::new");
            canvas.ensure_target(hwnd, 96).expect("ensure_target");
            canvas.resize(300, 60, 96).expect("resize");

            let width = canvas
                .measure(FONT_TEXT, 24.0, "你好 world")
                .expect("measure");
            assert!(width > 0.0, "测量宽度必须为正");
            assert_eq!(
                width,
                canvas.measure(FONT_TEXT, 24.0, "你好 world").unwrap(),
                "相同槽位/字号/文本的测量必须稳定（格式缓存）"
            );
            assert!(
                canvas.measure(FONT_TEXT, 40.0, "你好 world").unwrap() > width,
                "字号增大后测量宽度必须增大"
            );

            canvas
                .replay(&[
                    DrawCommand::FillRect {
                        x: 0.0,
                        y: 0.0,
                        w: 300.0,
                        h: 60.0,
                        color: 0xFF_F8F8F8,
                    },
                    DrawCommand::StrokeRect {
                        x: 0.0,
                        y: 0.0,
                        w: 300.0,
                        h: 60.0,
                        color: 0xFF_111111,
                        width: 2.0,
                    },
                    DrawCommand::Text {
                        x: 10.0,
                        y: 8.0,
                        font: FONT_TEXT,
                        size: 24.0,
                        color: 0xFF_111111,
                        text: "你好".to_string(),
                        glow: (0.0, 0),
                    },
                ])
                .expect("replay");
            // 第二帧：画刷/格式走复用路径。
            canvas
                .replay(&[DrawCommand::FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 300.0,
                    h: 60.0,
                    color: 0xFF_202020,
                }])
                .expect("replay 第二帧");

            // 设备丢失路径：失效后重建目标，再回放空帧。
            canvas.invalidate_target();
            canvas.ensure_target(hwnd, 96).expect("ensure_target 重建");
            canvas.replay(&[]).expect("replay 空帧");

            let _ = DestroyWindow(hwnd);
            RoUninitialize();
        }
    }

    #[test]
    fn stroke_edges_clamp_and_degenerate() {
        let ok = stroke_edges(0.0, 0.0, 100.0, 50.0, 2.0);
        assert_eq!(
            ok,
            [
                D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 2.0
                },
                D2D_RECT_F {
                    left: 0.0,
                    top: 48.0,
                    right: 100.0,
                    bottom: 50.0
                },
                D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: 2.0,
                    bottom: 50.0
                },
                D2D_RECT_F {
                    left: 98.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 50.0
                }
            ]
        );
        // 宽度过大时被限制在半宽/半高内，不产生负矩形。
        let big = stroke_edges(0.0, 0.0, 100.0, 50.0, 40.0);
        assert_eq!(big[0].bottom, 25.0);
        assert_eq!(big[2].right, 25.0);
        // 非法尺寸退化为零矩形。
        assert_eq!(
            stroke_edges(0.0, 0.0, 0.0, 50.0, 2.0),
            [D2D_RECT_F::default(); 4]
        );
        assert_eq!(
            stroke_edges(0.0, 0.0, 100.0, 50.0, 0.0),
            [D2D_RECT_F::default(); 4]
        );
    }

    #[test]
    fn color_decoding_ignores_alpha() {
        // 0xAARRGGBB：A=0x40, R=0x80, G=0x10, B=0x20
        let c = to_color(0x40_80_10_20);
        assert!((c.r - 0x80 as f32 / 255.0).abs() < 1e-6);
        assert!((c.g - 0x10 as f32 / 255.0).abs() < 1e-6);
        assert!((c.b - 0x20 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(c.a, 1.0);
    }
}
