//! 在 Composition 绘图表面上回放 D2D 命令，并协调面板、背景与装饰层。
//! 字体和文本布局由 `resources` 创建；本模块只缓存与当前设备关联的位图，
//! 并在设备失效后丢弃这些缓存以便重建。Canvas 持有的 COM 对象只能在其
//! 创建窗口的 UI 线程使用。

use crate::d2d_bindings::*;
use crate::protocol::DrawCommand;
use std::collections::HashMap;
use windows_core::{Error, Result as WinResult};

/// 管理窗口的 Composition 呈现器、D2D 帧回放及装饰层状态。
///
/// 内部 COM 对象绑定到创建线程；所有方法都应在该 UI 线程调用。帧缓存只在
/// 成功绘制并结束表面绘制后更新；设备失效时需调用 [`Self::invalidate_target`]，
/// 以便放弃旧设备的呈现器和位图。
pub struct Canvas {
    /// 当前窗口的 Composition 呈现器；失效后为空，需重新调用 [`Self::ensure_target`]。
    presenter: Option<crate::composition::Presenter>,
    /// 最后应用的面板样式，用于判断现有帧是否仍可复用。
    panel: crate::protocol::PanelStyle,
    /// 面板内容区的 DIP 尺寸，与 `dpi` 一同决定呈现几何。
    content_size: (f32, f32),
    /// 当前显示比例；96 表示每 DIP 对应一个像素。
    dpi: u32,
    /// 最近成功回放的命令快照；相同命令可跳过无变化的绘制。
    last_frame: Option<Vec<DrawCommand>>,
    /// 最近成功提交到呈现器的装饰层场景。
    layers: crate::layers::LayerScene,
    /// 按资源 ID 缓存设备位图，并以弱引用跟踪资源所有者；资源释放后可清理缓存。
    images: HashMap<i32, (std::sync::Weak<crate::resources::Resource>, ID2D1Bitmap)>,
}

/// 将直通 Alpha 的 ARGB 整数转换为 D2D 浮点颜色。
///
/// D2D 会在写入预乘 Alpha 的 Composition 表面时处理预乘；这里不预先乘色值。
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
    /// 创建尚未绑定窗口或设备的空画布。
    ///
    /// DPI 初值为 96；调用者须先用 [`Self::ensure_target`] 创建呈现目标，才能绘制。
    pub fn new() -> WinResult<Self> {
        Ok(Self {
            presenter: None,
            panel: Default::default(),
            content_size: (1.0, 1.0),
            dpi: 96,
            last_frame: None,
            layers: Default::default(),
            images: HashMap::new(),
        })
    }

    /// 为窗口创建呈现目标（若尚不存在），并记录当前 DPI。
    ///
    /// 首次创建时从 `hwnd` 读取客户区尺寸，空客户区按至少 1 像素创建。调用者
    /// 必须在窗口所属 UI 线程及有效的 WinRT/COM apartment 中调用；底层错误原样返回。
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

    /// 更新窗口像素尺寸和 DPI，并通知已创建的呈现器。
    ///
    /// DPI 变化会使帧快照失效；没有呈现器时只更新本地 DPI。尺寸约束及呈现失败
    /// 由底层呈现器报告，错误不会被吞掉。
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

    /// 设置面板外观、内容区 DIP 尺寸和 DPI。
    ///
    /// 样式、尺寸或 DPI 变化会清除可复用帧。呈现器尚未创建时只保存配置；创建后
    /// 同步更新面板表面，底层错误向调用者返回。
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

    /// 丢弃设备关联的呈现器、位图缓存和帧快照，以便设备重建后重新创建。
    ///
    /// 此操作不清除已保存的面板配置或装饰层场景。
    pub fn invalidate_target(&mut self) {
        self.presenter = None;
        self.images.clear();
        self.last_frame = None;
    }

    /// 设置窗口背景材质；目标尚未创建时暂不提交。
    ///
    /// 系统不支持材质或创建材质失败时，回退策略由呈现器处理；COM 错误仍可能返回。
    pub fn set_backdrop(&mut self, style: crate::protocol::BackdropStyle) -> WinResult<()> {
        if let Some(presenter) = &mut self.presenter {
            presenter.set_backdrop(style)?;
        }
        Ok(())
    }

    /// 将已验证的装饰层场景提交给原生 Composition 视觉树。
    ///
    /// 调用方必须先处理运行时的 `Keep` 回滚语义。新表面会在修改可见视觉之前准备；
    /// 只有底层提交成功后，本地场景缓存才会替换。无目标或无效场景分别返回
    /// `E_UNEXPECTED`、`E_INVALIDARG`。
    pub fn set_layers(&mut self, scene: &crate::layers::LayerScene) -> WinResult<()> {
        if !scene.validate(self.dpi) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let scene = scene.clone();
        self.images
            .retain(|_, (owner, _)| owner.strong_count() != 0);
        let mut presenter = self
            .presenter
            .take()
            .ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?;
        let result = presenter.set_layers(&scene, |target, commands| {
            self.draw_commands(target, commands)
        });
        self.presenter = Some(presenter);
        result?;
        self.layers = scene;
        Ok(())
    }

    /// 清除装饰层并停止其原生动画，供宿主隐藏面板时调用。
    ///
    /// 底层清理失败时保留本地场景缓存并返回错误；无呈现器时仅清空本地场景。
    pub fn clear_layers(&mut self) -> WinResult<()> {
        if let Some(presenter) = &mut self.presenter {
            presenter.clear_layers()?;
        }
        self.layers = Default::default();
        Ok(())
    }

    /// 错误是否为 D2D 设备丢失（决定是否进入重试路径而非直接失败）。
    pub fn is_device_lost(error: &Error) -> bool {
        matches!(
            error.code(),
            D2DERR_RECREATE_TARGET | DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET
        )
    }

    /// 在透明表面回放一帧：开始绘制、清屏、执行命令并结束绘制。
    ///
    /// 与最近成功帧完全相同的命令列表直接复用 Composition 保留的内容。只有绘制及
    /// `EndDraw` 均成功才更新快照；任何错误均返回调用者，后续可重试。目标未创建时
    /// 返回 `E_UNEXPECTED`。
    pub fn replay(&mut self, commands: &[DrawCommand]) -> WinResult<()> {
        if self.last_frame.as_deref() == Some(commands) {
            return Ok(());
        }
        self.images
            .retain(|_, (owner, _)| owner.strong_count() != 0);
        let target = self
            .presenter
            .as_mut()
            .ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?
            .begin_draw()?;
        let result = self.draw_commands(&target, commands);
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

    /// 在给定目标上执行命令序列；调用者负责目标绘制周期的开始与结束。
    ///
    /// 每帧先创建所需画刷，再清除为透明并按序绘制。非有限坐标命令会跳过；资源
    /// 类型不匹配或状态栈下溢返回 `E_INVALIDARG`。图像位图按资源 ID 缓存，并以
    /// 弱引用判断资源是否仍存活；资源释放后会在后续回放时回收对应缓存。
    fn draw_commands(
        &mut self,
        target: &ID2D1RenderTarget,
        commands: &[DrawCommand],
    ) -> WinResult<()> {
        (|| unsafe {
            // BeginDraw 之前分配好本帧全部 D2D 资源（与 ten 主题一致）。
            let mut brushes: Vec<(u32, ID2D1SolidColorBrush)> = Vec::new();
            for command in commands {
                let color = match command {
                    DrawCommand::FillRect { color, .. }
                    | DrawCommand::FillRoundedRect { color, .. }
                    | DrawCommand::StrokeRect { color, .. }
                    | DrawCommand::Layout { color, .. } => *color,
                    DrawCommand::Image { .. }
                    | DrawCommand::PushClip(_)
                    | DrawCommand::PushTransform(_)
                    | DrawCommand::PopState => 0xffffffff,
                };
                if !brushes.iter().any(|(c, _)| *c == color) {
                    brushes.push((color, target.CreateSolidColorBrush(&to_color(color), None)?));
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
            let mut drawing = DrawStack::new(target);
            for command in commands {
                // 非有限坐标的命令（NaN/Inf）跳过：D2D 对此行为未定义。
                if !command.is_finite() {
                    continue;
                }
                let color = match command {
                    DrawCommand::FillRect { color, .. }
                    | DrawCommand::FillRoundedRect { color, .. }
                    | DrawCommand::StrokeRect { color, .. }
                    | DrawCommand::Layout { color, .. } => *color,
                    DrawCommand::Image { .. }
                    | DrawCommand::PushClip(_)
                    | DrawCommand::PushTransform(_)
                    | DrawCommand::PopState => 0xffffffff,
                };
                let brush = brush(color)?;
                match command {
                    DrawCommand::PushTransform(matrix) => drawing.transform(*matrix)?,
                    DrawCommand::PushClip(rect) => drawing.clip(*rect),
                    DrawCommand::PopState => drawing.pop()?,
                    DrawCommand::Image {
                        resource,
                        x,
                        y,
                        w,
                        h,
                        opacity,
                    } => {
                        let crate::resources::Kind::Image {
                            pixels,
                            width,
                            height,
                        } = &resource.kind
                        else {
                            return Err(Error::from_hresult(E_INVALIDARG));
                        };
                        if !self.images.contains_key(&resource.id) {
                            let bitmap = target.CreateBitmap(
                                D2D_SIZE_U {
                                    width: *width,
                                    height: *height,
                                },
                                Some(pixels.as_ptr().cast()),
                                *width * 4,
                                &D2D1_BITMAP_PROPERTIES {
                                    pixelFormat: D2D1_PIXEL_FORMAT {
                                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                                    },
                                    dpiX: 96.0,
                                    dpiY: 96.0,
                                },
                            )?;
                            self.images
                                .insert(resource.id, (std::sync::Arc::downgrade(resource), bitmap));
                        }
                        let bitmap = &self.images[&resource.id].1;
                        target.DrawBitmap(
                            bitmap,
                            Some(&D2D_RECT_F {
                                left: *x,
                                top: *y,
                                right: x + w,
                                bottom: y + h,
                            }),
                            *opacity,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    }
                    DrawCommand::Layout {
                        resource,
                        x,
                        y,
                        glow,
                        ..
                    } => {
                        let crate::resources::Kind::Layout { layout, .. } = &resource.kind else {
                            return Err(Error::from_hresult(E_INVALIDARG));
                        };
                        if glow.0 > 0.0 && glow.1 >> 24 != 0 {
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
                                        layout,
                                        &halo,
                                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                                    );
                                }
                            }
                        }
                        target.DrawTextLayout(
                            windows_numerics::Vector2 { x: *x, y: *y },
                            layout,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
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
                }
            }
            Ok(())
        })()
    }
}

/// 追踪绘制命令压入的裁剪和变换，并在离开作用域时恢复 D2D 状态。
///
/// `base` 保存 Composition 在 `BeginDraw` 时设置的表面偏移变换，析构时无论绘制
/// 是否成功都会恢复它；栈项为 `Some` 时保存旧变换，为 `None` 时代表一个裁剪。
struct DrawStack<'a> {
    /// 借用的目标；其生命周期覆盖整个状态栈。
    target: &'a ID2D1RenderTarget,
    /// 进入本次命令回放前的目标变换，不可丢弃 Composition 的表面偏移。
    base: windows_numerics::Matrix3x2,
    /// 与 Push 操作一一对应的旧变换或裁剪标记，Pop 按后进先出恢复。
    stack: Vec<Option<windows_numerics::Matrix3x2>>,
}
impl<'a> DrawStack<'a> {
    /// 记录目标初始变换；之后的状态恢复均以此为基准。
    fn new(target: &'a ID2D1RenderTarget) -> Self {
        let mut base = windows_numerics::Matrix3x2::default();
        unsafe { target.GetTransform(&mut base) };
        Self {
            target,
            base,
            stack: Vec::new(),
        }
    }
    /// 将有限且幅值受限的矩阵与当前目标变换合成后压栈并应用。
    ///
    /// 非有限分量或绝对值超过 1e9 时返回 `E_INVALIDARG`，且不会改变目标或栈。
    fn transform(&mut self, m: [f32; 6]) -> WinResult<()> {
        let mut p = windows_numerics::Matrix3x2::default();
        unsafe { self.target.GetTransform(&mut p) };
        let next = windows_numerics::Matrix3x2 {
            m11: m[0] * p.m11 + m[1] * p.m21,
            m12: m[0] * p.m12 + m[1] * p.m22,
            m21: m[2] * p.m11 + m[3] * p.m21,
            m22: m[2] * p.m12 + m[3] * p.m22,
            m31: m[4] * p.m11 + m[5] * p.m21 + p.m31,
            m32: m[4] * p.m12 + m[5] * p.m22 + p.m32,
        };
        if [next.m11, next.m12, next.m21, next.m22, next.m31, next.m32]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e9)
        {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        self.stack.push(Some(p));
        unsafe { self.target.SetTransform(&next) };
        Ok(())
    }
    /// 压入轴对齐裁剪并记录对应的栈标记，供后续 [`Self::pop`] 配对恢复。
    fn clip(&mut self, r: crate::protocol::Rect) {
        unsafe {
            self.target.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: r.x,
                    top: r.y,
                    right: r.x + r.w,
                    bottom: r.y + r.h,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            )
        };
        self.stack.push(None);
    }
    /// 弹出并恢复一个变换或裁剪；空栈表示命令序列不平衡，返回 `E_INVALIDARG`。
    fn pop(&mut self) -> WinResult<()> {
        match self
            .stack
            .pop()
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?
        {
            Some(matrix) => unsafe { self.target.SetTransform(&matrix) },
            None => unsafe { self.target.PopAxisAlignedClip() },
        }
        Ok(())
    }
}
impl Drop for DrawStack<'_> {
    fn drop(&mut self) {
        while !self.stack.is_empty() {
            let _ = self.pop();
        }
        unsafe { self.target.SetTransform(&self.base) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::comrt::WinRtApartment;
    use windows_strings::{PCWSTR, w};

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
        let _apartment = WinRtApartment::initialize_sta().expect("COM apartment 初始化失败");
        unsafe {
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

            let mut resources = crate::resources::Resources::default();
            let font = resources.font("Microsoft YaHei UI", 24.0, 400).unwrap();
            let layout = resources
                .layout(font, "你好 world", 300.0, 60.0, false as i32)
                .unwrap();
            let layout = resources.get(layout).unwrap();
            let crate::resources::Kind::Layout { metrics, .. } = &layout.kind else {
                panic!("layout expected")
            };
            assert!(metrics[0] > 0.0 && metrics[1] > 0.0 && metrics[2] > 0.0);
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, 1, 1);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&[255, 0, 0, 128])
                    .unwrap();
            }
            let image = resources.image(&png).unwrap();
            let image = resources.get(image).unwrap();

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
                    DrawCommand::PushTransform([1.0, 0.0, 0.0, 1.0, 10.0, 8.0]),
                    DrawCommand::PushClip(crate::protocol::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 250.0,
                        h: 40.0,
                    }),
                    DrawCommand::Layout {
                        resource: layout,
                        x: 0.0,
                        y: 0.0,
                        color: 0xff111111,
                        glow: (0.0, 0),
                    },
                    DrawCommand::PopState,
                    DrawCommand::PopState,
                    DrawCommand::Image {
                        resource: image,
                        x: 260.0,
                        y: 0.0,
                        w: 40.0,
                        h: 60.0,
                        opacity: 0.8,
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

            use crate::layers::{
                LayerMotion, LayerOperation, LayerProperty, LayerScene, LayerState,
            };
            let start = std::time::Instant::now();
            let mut scene = LayerScene {
                layers: vec![LayerState {
                    z_index: 0,
                    generation: 1,
                    clip: None,
                    interactive: false,
                    regions: Vec::new(),
                    id: 1,
                    size: (32.0, 32.0),
                    commands: vec![DrawCommand::FillRect {
                        x: 0.0,
                        y: 0.0,
                        w: 32.0,
                        h: 32.0,
                        color: 0x80ff0000,
                    }],
                    motions: vec![LayerMotion {
                        snap_from: false,
                        property: LayerProperty::Opacity,
                        easing: crate::abi::Easing::SmoothStep,
                        revision: 1,
                        operation: LayerOperation::Animate,
                        from: 0.0,
                        to: 1.0,
                        start,
                        deadline: start + std::time::Duration::from_secs(1),
                    }],
                }],
            };
            canvas.set_layers(&scene).expect("native decoration");
            // 单轴静态写入与另一轴动画共存，不写回整个 Offset 向量。
            let mut vertical = scene.layers[0].motions[0];
            vertical.property = LayerProperty::OffsetY;
            vertical.from = 5.0;
            vertical.to = 1.0;
            let mut horizontal = vertical;
            horizontal.property = LayerProperty::OffsetX;
            horizontal.operation = LayerOperation::Set;
            horizontal.to = 80.0;
            scene.layers[0].motions.extend([vertical, horizontal]);
            canvas.set_layers(&scene).expect("independent axes");
            scene.layers[0].clip = Some(crate::protocol::Rect {
                x: 5.0,
                y: 5.0,
                w: 40.0,
                h: 30.0,
            });
            let objects = canvas.presenter.as_ref().unwrap().layer_objects(1);
            canvas.set_layers(&scene).expect("retained revision");
            assert_eq!(objects, canvas.presenter.as_ref().unwrap().layer_objects(1));
            scene.layers[0].motions[2].revision += 10;
            scene.layers[0].motions[2].operation = LayerOperation::Animate;
            scene.layers[0].motions[2].from = 80.0;
            scene.layers[0].motions[2].to = 120.0;
            canvas
                .set_layers(&scene)
                .expect("native slide reuses surface");
            assert_eq!(objects, canvas.presenter.as_ref().unwrap().layer_objects(1));
            canvas
                .set_panel(Default::default(), (240.0, 48.0), 96)
                .unwrap();
            canvas
                .set_layers(&scene)
                .expect("clip resize retains animation");
            assert_eq!(objects, canvas.presenter.as_ref().unwrap().layer_objects(1));
            scene.layers[0].motions[0].revision = 2;
            scene.layers[0].motions[0].to = 0.4;
            canvas
                .set_layers(&scene)
                .expect("retarget from presentation");
            assert_eq!(objects, canvas.presenter.as_ref().unwrap().layer_objects(1));
            canvas.invalidate_target();
            canvas.ensure_target(hwnd, 96).unwrap();
            canvas.replay(&[]).unwrap();
            canvas
                .set_layers(&scene)
                .expect("restore decoration deadline");
            assert_ne!(objects, canvas.presenter.as_ref().unwrap().layer_objects(1));
            scene.layers[0].motions[0].revision = 3;
            scene.layers[0].motions[0].operation = LayerOperation::StopCurrent;
            canvas.set_layers(&scene).expect("stop at presentation");
            scene.layers[0].motions[0].revision = 4;
            scene.layers[0].motions[0].operation = LayerOperation::StopEnd;
            canvas.set_layers(&scene).expect("stop at target");
            scene.layers[0].id = 0;
            assert!(canvas.set_layers(&scene).is_err());
            assert_eq!(canvas.layers.layers[0].id, 1);
            canvas.clear_layers().expect("hide clears decorations");
            assert!(canvas.layers.layers.is_empty());
            drop(canvas);

            let _ = DestroyWindow(hwnd);
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
}
