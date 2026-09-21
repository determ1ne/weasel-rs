//! Composition透明表面的D2D回放。字体/文本测量属于resources模块，
//! 此处仅缓存设备相关图片并回放已验证的资源命令，设备丢失后可重建。
//! 所有调用都在创建窗口的UI线程，不能跨线程使用COM资源。

use crate::d2d_bindings::*;
use crate::protocol::DrawCommand;
use std::collections::HashMap;
use windows_core::{Error, Result as WinResult};

/// D2D/DWrite 渲染器。COM 接口不 `Send`，实例必须留在创建线程上。
pub struct Canvas {
    presenter: Option<crate::composition::Presenter>,
    panel: crate::protocol::PanelStyle,
    content_size: (f32, f32),
    dpi: u32,
    last_frame: Option<Vec<DrawCommand>>,
    layers: crate::layers::LayerScene,
    images: HashMap<i32, (std::sync::Weak<crate::resources::Resource>, ID2D1Bitmap)>,
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
        self.images.clear();
        self.last_frame = None;
    }

    pub fn set_backdrop(&mut self, style: crate::protocol::BackdropStyle) -> WinResult<()> {
        if let Some(presenter) = &mut self.presenter {
            presenter.set_backdrop(style)?;
        }
        Ok(())
    }

    /// Present-only commit. Runtime must roll back Keep before calling this.
    /// Surface preparation completes before any decoration visual is changed.
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

    /// Host hide: stop all native motion and discard decoration surfaces.
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

    /// 回放一帧命令：`BeginDraw` → 清屏 → 逐条绘制 → `EndDraw`。
    /// Transparent surface; unchanged command lists reuse the compositor's retained content.
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

/// 失败路径同样恢复裁剪与变换；不能把Composition提供的表面偏移覆盖掉。
struct DrawStack<'a> {
    target: &'a ID2D1RenderTarget,
    base: windows_numerics::Matrix3x2,
    stack: Vec<Option<windows_numerics::Matrix3x2>>,
}
impl<'a> DrawStack<'a> {
    fn new(target: &'a ID2D1RenderTarget) -> Self {
        let mut base = windows_numerics::Matrix3x2::default();
        unsafe { target.GetTransform(&mut base) };
        Self {
            target,
            base,
            stack: Vec::new(),
        }
    }
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
    use crate::bindings::Windows::Win32::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
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
}
