//! 在 UI 线程上创建并维护 Windows Composition 与 D2D 呈现资源。
//! 窗口尺寸以物理像素计，面板尺寸和图层几何以 DIP 计，并按 DPI 转换。
//! [`Presenter::begin_draw`] 返回的目标只在配对的 [`Presenter::end_draw`] 前有效；
//! 调用方不得自行调用目标的 BeginDraw/EndDraw，也不得覆盖表面提供的图集偏移变换。
use crate::d2d_bindings::{Windows, *};
use crate::layers::{LayerMotion, LayerOperation, LayerProperty, LayerScene};
use Windows::Foundation::Size;
use Windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use Windows::System::{DispatcherQueue, DispatcherQueueController};
use Windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, Compositor, ContainerVisual,
    Desktop::DesktopWindowTarget, DropShadow, SpriteVisual,
};
use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use std::{collections::HashMap, time::Instant};
use weasel_common::comrt::WinRtApartment;
use windows_core::{Interface, Result};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

#[derive(Clone)]
/// 一层装饰内容对应的原生视觉、绘图表面及可复用状态快照。
///
/// 克隆会增加底层 COM 对象引用；缓存只在成功提交整场景后替换。`generation`
/// 标识层实例，换代时必须重新关联子视觉并停止旧实例动画。
struct Decoration {
    /// 层实例代数；同 ID 但代数不同表示销毁后重建。
    generation: u64,
    /// 显示层位图的 SpriteVisual，可独立应用偏移、缩放、透明度动画。
    visual: SpriteVisual,
    /// 承载子视觉和静态裁剪的父容器，裁剪不随子视觉变换。
    container: ContainerVisual,
    /// 相对装饰根节点、以 DIP 表示的可选裁剪区域。
    clip: Option<crate::protocol::Rect>,
    /// 子视觉显示的 Composition 绘图表面。
    surface: CompositionDrawingSurface,
    /// 生成当前表面的命令快照，用于判断是否需要重绘。
    commands: Vec<crate::protocol::DrawCommand>,
    /// 表面逻辑尺寸，单位为 DIP。
    size: (f32, f32),
    /// 创建当前表面时使用的 DPI；变化时需重新绘制并更新裁剪。
    dpi: u32,
    /// 每个视觉属性最近提交的运动描述，用于识别重复命令并延续动画。
    motions: HashMap<LayerProperty, LayerMotion>,
}

/// 将协议层属性映射为 Composition 可寻址的动画属性名。
fn property_name(property: LayerProperty) -> windows_core::HSTRING {
    match property {
        LayerProperty::Opacity => "Opacity",
        LayerProperty::OffsetX => "Offset.X",
        LayerProperty::OffsetY => "Offset.Y",
        LayerProperty::ScaleX => "Scale.X",
        LayerProperty::ScaleY => "Scale.Y",
    }
    .into()
}

/// 停止指定属性上的动画，并设置它的静态值。
///
/// 对 Offset 和 Scale 只写入一个轴，以免覆盖另一轴正在运行的动画；Composition
/// 错误直接返回。
fn set_value(visual: &SpriteVisual, property: LayerProperty, value: f32) -> Result<()> {
    visual.StopAnimation(&property_name(property))?;
    match property {
        LayerProperty::Opacity => visual.SetOpacity(value),
        LayerProperty::OffsetX | LayerProperty::OffsetY => set_axis(visual, property, value),
        LayerProperty::ScaleX | LayerProperty::ScaleY => set_axis(visual, property, value),
    }
}

/// 通过常量表达式仅设置 Offset 或 Scale 的一个轴。
///
/// 不读写整个向量：getter 不提供动画的实时采样值，而整向量写入会干扰另一轴的
/// 动画。表达式由原生 compositor 求值，不需要逐帧回调 WASM。
fn set_axis(visual: &SpriteVisual, property: LayerProperty, value: f32) -> Result<()> {
    let expression = visual
        .Compositor()?
        .CreateExpressionAnimationWithExpression(&"value".into())?;
    expression.SetScalarParameter(&"value".into(), value)?;
    visual.StartAnimation(&property_name(property), &expression)
}

/// 本模块创建的 DispatcherQueue 及其线程 apartment 所有权。
///
/// 若线程已有队列则不会构造此对象，也不会关闭外部队列。字段顺序保证控制器先
/// 释放，随后释放 apartment 令牌。
struct OwnedQueue {
    /// 队列控制器；析构时请求异步关闭本模块拥有的队列。
    controller: DispatcherQueueController,
    // Released after the controller, including on thread teardown.
    _apartment: WinRtApartment,
}
impl Drop for OwnedQueue {
    fn drop(&mut self) {
        // Only a queue created here is shut down, never an existing XAML queue.
        // Shutdown is asynchronous; never block the UI thread waiting for it.
        let _ = self.controller.ShutdownQueueAsync();
    }
}

// A dispatcher belongs to the thread, not to a transient presenter (device-loss
// recovery may replace presenters while other composition clients still exist).
thread_local! {
    static QUEUE: RefCell<Option<OwnedQueue>> = const { RefCell::new(None) };
}

/// 确保当前线程存在 DispatcherQueue。
///
/// 复用线程已存在的队列；否则创建并在线程局部槽中保留控制器及 apartment，避免
/// Presenter 因设备重建而销毁仍被其他 Composition 客户端使用的队列。
fn ensure_queue() -> Result<()> {
    if DispatcherQueue::GetForCurrentThread().is_ok() {
        return Ok(());
    }
    let apartment = WinRtApartment::initialize_sta()?;
    let controller: DispatcherQueueController = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        })?
        .cast()?
    };
    QUEUE.with(|slot| {
        *slot.borrow_mut() = Some(OwnedQueue {
            controller,
            _apartment: apartment,
        })
    });
    Ok(())
}

/// Composition 绘图表面的一次 BeginDraw/EndDraw 配对及其异常清理守卫。
struct SurfaceDraw {
    /// 与本次 BeginDraw 配对的互操作接口。
    interop: ICompositionDrawingSurfaceInterop,
    /// 标记 EndDraw 是否已调用；即使 EndDraw 报错也不能重试。
    ended: bool,
}
impl SurfaceDraw {
    /// 开始绘制表面并设置 DPI 与 Composition 返回的图集偏移变换。
    ///
    /// 返回的目标借助共享底层接口引用表面绘图上下文，只应在守卫结束前使用。创建
    /// 目标转换失败时，守卫析构仍会结束已经开始的绘制。
    fn begin(surface: &CompositionDrawingSurface, dpi: u32) -> Result<(Self, ID2D1RenderTarget)> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        let context: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let guard = Self {
            interop,
            ended: false,
        };
        let target: ID2D1RenderTarget = context.cast()?;
        let scale = dpi.max(1) as f32 / 96.0;
        unsafe {
            target.SetDpi(dpi.max(1) as f32, dpi.max(1) as f32);
            target.SetTransform(&Matrix3x2 {
                m11: 1.0,
                m12: 0.0,
                m21: 0.0,
                m22: 1.0,
                m31: offset.x as f32 / scale,
                m32: offset.y as f32 / scale,
            });
        }
        Ok((guard, target))
    }
    /// 结束本次绘制且只调用一次 EndDraw；返回系统报告的结束错误。
    fn finish(mut self) -> Result<()> {
        self.ended = true;
        let result = unsafe { self.interop.EndDraw().ok() };
        // EndDraw must not be retried, even when it reports a lost device.
        result
    }
}
impl Drop for SurfaceDraw {
    /// 遇到提前返回或 panic 时尽力配对 EndDraw，避免表面停留在绘制状态。
    fn drop(&mut self) {
        if !self.ended {
            unsafe {
                let _ = self.interop.EndDraw();
            }
        }
    }
}

/// 将 D2D 内容、面板装饰与背景绑定到一个 HWND 的 Composition 呈现器。
///
/// 所有 COM 对象和方法调用都限于创建它的 UI 线程。绘制事务期间只能调用
/// [`Self::end_draw`]；其他要求空闲状态的操作会以 `E_UNEXPECTED` 失败。Presenter
/// 不拥有 HWND，析构时只解除 Composition target 的根视觉。
pub struct Presenter {
    /// 正在进行的内容表面绘制事务；存在时呈现器处于非空闲状态。
    drawing: Option<SurfaceDraw>,
    /// 按层 ID 保存最近成功提交的原生装饰对象及其缓存状态。
    decorations: HashMap<u32, Decoration>,
    /// 装饰层父节点，裁剪区域按窗口内容区设置。
    decoration_root: ContainerVisual,
    /// 当前装饰层从底到顶的 ID 顺序快照。
    decoration_order: Vec<u32>,
    /// HWND 的 Composition target；持有底层目标但不取得窗口所有权。
    target: DesktopWindowTarget,
    /// 管理内容、背景、阴影和装饰根节点的顶层视觉。
    root: ContainerVisual,
    /// 绘制主内容表面的视觉。
    content: SpriteVisual,
    /// 使用面板形状遮罩呈现背景材质的视觉。
    backdrop: SpriteVisual,
    /// 最近成功应用的背景样式；相同样式可跳过重复配置。
    backdrop_style: Option<crate::protocol::BackdropStyle>,
    /// 创建呈现器时绑定的 HWND，用于启用系统背景材质。
    hwnd: HWND,
    /// 承载投影效果的视觉。
    shadow_visual: SpriteVisual,
    /// 面板投影及其形状遮罩配置。
    shadow: DropShadow,
    /// 主内容绘制目标，像素格式为预乘 Alpha BGRA。
    surface: CompositionDrawingSurface,
    /// 面板轮廓遮罩，同时用于阴影和背景材质裁切。
    mask: CompositionDrawingSurface,
    /// 必须与 D2D/D3D 设备及 compositor 同寿命的 Composition 图形设备。
    _graphics: CompositionGraphicsDevice,
    /// 拥有视觉树和表面的 compositor。
    _compositor: Compositor,
    /// 为 Composition 图形设备提供的 D2D 设备。
    _d2d: ID2D1Device,
    /// 为 D2D 设备提供 DXGI 设备的 D3D 设备。
    _d3d: ID3D11Device,
    /// 创建 D2D 设备所用的单线程工厂。
    _factory: ID2D1Factory1,
    /// 当前 DPI，零值输入规范化为 1。
    dpi: u32,
    /// 面板表面与遮罩当前的物理像素尺寸。
    surface_size: (u32, u32),
    /// 已成功应用的面板样式、DIP 尺寸和请求 DPI 缓存。
    panel: Option<(crate::protocol::PanelStyle, f32, f32, u32)>,
    /// 仅用于类型系统，禁止将线程绑定的 Presenter 发送到其他线程。
    _ui_thread: PhantomData<Rc<()>>,
    // Rust drops fields in declaration order: COM resources must go first.
    _apartment: WinRtApartment,
}

impl Presenter {
    /// 为已有窗口创建 GPU、Composition 目标及初始视觉树。
    ///
    /// 调用方拥有 `hwnd`，并负责在其 UI 线程运行消息循环；构造器在当前线程初始化
    /// WinRT apartment 和调度队列。`width`、`height` 是像素，超出表面限制时返回
    /// `E_INVALIDARG`，其余系统创建错误原样返回。
    pub fn new(hwnd: HWND, dpi: u32, width: u32, height: u32) -> Result<Self> {
        check_pixels(width, height)?;
        let apartment = WinRtApartment::initialize_sta()?;
        ensure_queue()?;
        let mut d3d = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT as u32,
                None,
                D3D11_SDK_VERSION as u32,
                Some(&mut d3d),
                None,
                None,
            )
            .ok()?;
        }
        let d3d = d3d.ok_or_else(|| windows_core::Error::from_hresult(E_UNEXPECTED))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let d2d = unsafe { factory.CreateDevice(&dxgi)? };
        let compositor = Compositor::new()?;
        let native: ICompositorInterop = compositor.cast()?;
        let graphics: CompositionGraphicsDevice =
            unsafe { native.CreateGraphicsDevice(&d2d)?.cast()? };
        let desktop: ICompositorDesktopInterop = compositor.cast()?;
        let target: DesktopWindowTarget =
            unsafe { desktop.CreateDesktopWindowTarget(hwnd, false)?.cast()? };
        let root = compositor.CreateContainerVisual()?;
        let content = compositor.CreateSpriteVisual()?;
        let decoration_root = compositor.CreateContainerVisual()?;
        decoration_root.SetClip(&compositor.CreateInsetClip()?)?;
        let backdrop = compositor.CreateSpriteVisual()?;
        let shadow_visual = compositor.CreateSpriteVisual()?;
        let shadow = compositor.CreateDropShadow()?;
        let make_surface = || {
            graphics.CreateDrawingSurface(
                Size {
                    Width: 1.0,
                    Height: 1.0,
                },
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )
        };
        let surface = make_surface()?;
        let mask = make_surface()?;
        content.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        shadow.SetMask(&compositor.CreateSurfaceBrushWithSurface(&mask)?)?;
        shadow_visual.SetShadow(&shadow)?;
        root.Children()?.InsertAtBottom(&shadow_visual)?;
        root.Children()?.InsertAtTop(&backdrop)?;
        root.Children()?.InsertAtTop(&content)?;
        root.Children()?.InsertAtTop(&decoration_root)?;
        target.SetRoot(&root)?;
        let mut presenter = Self {
            drawing: None,
            decorations: HashMap::new(),
            decoration_root,
            decoration_order: Vec::new(),
            target,
            root,
            content,
            backdrop,
            backdrop_style: None,
            hwnd,
            shadow_visual,
            shadow,
            surface,
            mask,
            _graphics: graphics,
            _compositor: compositor,
            _d2d: d2d,
            _d3d: d3d,
            _factory: factory,
            dpi: dpi.max(1),
            surface_size: (1, 1),
            panel: None,
            _ui_thread: PhantomData,
            _apartment: apartment,
        };
        presenter.resize(width, height, dpi)?;
        Ok(presenter)
    }

    /// 更新顶层视觉的像素尺寸和呈现 DPI。
    ///
    /// 绘制事务未结束时拒绝执行；像素尺寸超限返回 `E_INVALIDARG`。DPI 改变会使
    /// 背景缓存失效，以便下次设置时按新比例重建。
    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<()> {
        self.idle()?;
        check_pixels(width, height)?;
        self.root.SetSize(Vector2 {
            x: width as f32,
            y: height as f32,
        })?;
        if self.dpi != dpi.max(1) {
            self.backdrop_style = None;
        }
        self.dpi = dpi.max(1);
        Ok(())
    }

    /// 应用面板几何、圆角、投影和内容偏移，并重绘面板形状遮罩。
    ///
    /// `width`、`height` 及样式几何以 DIP 表示，表面尺寸向上取整到物理像素。非法
    /// 数值、非正尺寸、零 DPI 或超限表面返回 `E_INVALIDARG`。提交开始前会使旧缓存
    /// 失效；部分 COM 更新失败后不会把旧样式误记为有效。
    pub fn set_panel(
        &mut self,
        style: &crate::protocol::PanelStyle,
        width: f32,
        height: f32,
        dpi: u32,
    ) -> Result<()> {
        self.idle()?;
        if ![
            width,
            height,
            style.corner_radius,
            style.shadow_radius,
            style.offset_x,
            style.offset_y,
        ]
        .iter()
        .all(|v| v.is_finite())
            || width <= 0.0
            || height <= 0.0
            || dpi == 0
            || style.corner_radius < 0.0
            || style.shadow_radius < 0.0
        {
            return Err(windows_core::Error::from_hresult(E_INVALIDARG));
        }
        if self.panel.as_ref() == Some(&(*style, width, height, dpi)) {
            return Ok(());
        }
        let scale = dpi.max(1) as f32 / 96.0;
        let pixels = (
            (width * scale).ceil().max(1.0) as u32,
            (height * scale).ceil().max(1.0) as u32,
        );
        check_pixels(pixels.0, pixels.1)?;
        // A failed partial update must not make the old panel cache look valid.
        self.panel = None;
        if pixels != self.surface_size {
            let size = SIZE {
                cx: pixels.0 as i32,
                cy: pixels.1 as i32,
            };
            unsafe {
                self.surface
                    .cast::<ICompositionDrawingSurfaceInterop>()?
                    .Resize(size)
                    .ok()?;
                self.mask
                    .cast::<ICompositionDrawingSurfaceInterop>()?
                    .Resize(size)
                    .ok()?;
            }
            self.surface_size = pixels;
        }
        let inset = crate::geometry::insets(style);
        let offset = Vector3 {
            x: inset.left * scale,
            y: inset.top * scale,
            z: 0.0,
        };
        let size = Vector2 {
            x: pixels.0 as f32,
            y: pixels.1 as f32,
        };
        self.content.SetSize(size)?;
        self.decoration_root.SetSize(size)?;
        self.content.SetOffset(offset)?;
        self.decoration_root.SetOffset(offset)?;
        self.backdrop.SetSize(size)?;
        self.backdrop.SetOffset(offset)?;
        self.shadow_visual.SetSize(size)?;
        self.shadow_visual.SetOffset(offset)?;
        self.shadow.SetBlurRadius(style.shadow_radius * scale)?;
        self.shadow.SetOffset(Vector3 {
            x: style.offset_x * scale,
            y: style.offset_y * scale,
            z: 0.0,
        })?;
        self.shadow.SetColor(Windows::UI::Color {
            A: 255,
            R: (style.color >> 16) as u8,
            G: (style.color >> 8) as u8,
            B: style.color as u8,
        })?;
        self.shadow.SetOpacity(if style.shadow_radius > 0.0 {
            (style.color >> 24) as f32 / 255.0
        } else {
            0.0
        })?;
        let (draw, target) = SurfaceDraw::begin(&self.mask, dpi)?;
        unsafe {
            target.Clear(Some(&D2D_COLOR_F::default()));
            let white = target.CreateSolidColorBrush(
                &D2D_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            )?;
            let rect = style.bounds.unwrap_or(crate::protocol::Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: height,
            });
            let radius = style
                .corner_radius
                .max(0.0)
                .min(rect.w / 2.0)
                .min(rect.h / 2.0);
            target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: rect.x,
                        top: rect.y,
                        right: rect.x + rect.w,
                        bottom: rect.y + rect.h,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &white,
            );
        }
        draw.finish()?;
        self.dpi = dpi.max(1);
        self.panel = Some((*style, width, height, dpi));
        Ok(())
    }

    /// 为面板设置系统背景材质或纯色回退，并使用面板遮罩裁切。
    ///
    /// 禁用时移除背景画刷；启用时系统材质不可用会记录一次警告并创建回退色画刷。
    /// 只有完整应用成功才更新样式缓存；COM 配置错误返回调用方。
    pub fn set_backdrop(&mut self, style: crate::protocol::BackdropStyle) -> Result<()> {
        if self.backdrop_style == Some(style) {
            return Ok(());
        }
        if !style.enabled {
            self.backdrop.SetBrush(None)?;
        } else {
            // Windows 10 can present the theme but lacks this documented HWND opt-in.
            let brush = (|| -> Result<Windows::UI::Composition::CompositionBrush> {
                if windows_version::OsVersion::current()
                    < windows_version::OsVersion::new(10, 0, 0, 22000)
                {
                    return Err(windows_core::Error::new(
                        E_UNEXPECTED,
                        "host backdrop requires Windows 11",
                    ));
                }
                let enabled = windows_core::BOOL(1);
                unsafe {
                    DwmSetWindowAttribute(
                        self.hwnd,
                        DWMWA_USE_HOSTBACKDROPBRUSH as u32,
                        &enabled as *const _ as _,
                        size_of_val(&enabled) as u32,
                    )
                    .ok()?;
                }
                let mut pixel_style = style;
                pixel_style.blur_sigma *= self.dpi as f32 / 96.0;
                crate::glass::create_brush(&self._compositor, &pixel_style)
            })();
            let brush = match brush {
                Ok(brush) => brush,
                Err(error) => {
                    // One warning per material setup, not per paint.
                    crate::glass::record(
                        weasel_common::logging::Level::WARN,
                        format_args!("glass backdrop unavailable: {error}; using solid fallback"),
                    );
                    self._compositor
                        .CreateColorBrushWithColor(Windows::UI::Color {
                            A: 255,
                            R: (style.fallback_color >> 16) as u8,
                            G: (style.fallback_color >> 8) as u8,
                            B: style.fallback_color as u8,
                        })?
                        .cast()?
                }
            };
            let mask = self._compositor.CreateMaskBrush()?;
            mask.SetSource(&brush)?;
            mask.SetMask(&self._compositor.CreateSurfaceBrushWithSurface(&self.mask)?)?;
            self.backdrop.SetBrush(&mask)?;
        }
        self.backdrop_style = Some(style);
        Ok(())
    }

    /// 开始主内容表面的绘制事务并返回 D2D 目标。
    ///
    /// 调用者必须随后且仅随后调用一次 [`Self::end_draw`]；在事务期间不得调用其他
    /// 需要空闲状态的呈现操作。目标不得逃逸到事务生命周期之外，也不得自行结束绘制。
    pub fn begin_draw(&mut self) -> Result<ID2D1RenderTarget> {
        self.idle()?;
        let (draw, target) = SurfaceDraw::begin(&self.surface, self.dpi)?;
        self.drawing = Some(draw);
        Ok(target)
    }

    /// 停止所有装饰视觉动画并从视觉树和缓存中移除装饰层。
    ///
    /// 若停止动画或修改视觉树失败，立即返回系统错误；仅在清理步骤成功后清空缓存。
    pub fn clear_layers(&mut self) -> Result<()> {
        self.idle()?;
        for layer in self.decorations.values() {
            for property in layer.motions.keys() {
                layer.visual.StopAnimation(&property_name(*property))?;
            }
        }
        self.decoration_root.Children()?.RemoveAll()?;
        self.decorations.clear();
        self.decoration_order.clear();
        Ok(())
    }

    /// 准备并提交装饰层场景，成功后替换本地层缓存。
    ///
    /// 先验证场景并在脱离可见树的表面上完成必要重绘，再提交视觉属性和顺序。相同
    /// 代数且内容未变的层复用表面、视觉及仍在运行的动画。绘制回调只在此调用期间
    /// 借用目标；准备失败不会提交新缓存，COM 提交部分失败则清除装饰树以避免缓存
    /// 谎报状态。无效场景返回 `E_INVALIDARG`，绘制事务未结束返回 `E_UNEXPECTED`。
    ///
    /// 偏移动画按当前 DPI 换算为像素，其他属性保持无量纲。过期动画落为终值；活动
    /// 动画按截止时间设置原生时长，不依赖逐帧宿主回调。
    pub fn set_layers(
        &mut self,
        scene: &LayerScene,
        mut draw: impl FnMut(&ID2D1RenderTarget, &[crate::protocol::DrawCommand]) -> Result<()>,
    ) -> Result<()> {
        self.idle()?;
        if !scene.validate(self.dpi) {
            return Err(windows_core::Error::from_hresult(E_INVALIDARG));
        }
        let scale = self.dpi as f32 / 96.0;
        // Prepare detached surfaces before touching the visible tree. Unchanged
        // layers retain both surface and visual, including running animations.
        let mut prepared = HashMap::new();
        for state in &scene.layers {
            let old = self
                .decorations
                .get(&state.id)
                .filter(|old| old.generation == state.generation);
            let redraw = old.is_none_or(|l| {
                l.commands != state.commands || l.size != state.size || l.dpi != self.dpi
            });
            let surface = if redraw {
                let surface = self._graphics.CreateDrawingSurface(
                    Size {
                        Width: (state.size.0 * scale).ceil(),
                        Height: (state.size.1 * scale).ceil(),
                    },
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    DirectXAlphaMode::Premultiplied,
                )?;
                let (guard, target) = SurfaceDraw::begin(&surface, self.dpi)?;
                let result = draw(&target, &state.commands);
                let finish = guard.finish();
                result?;
                finish?;
                surface
            } else {
                old.unwrap().surface.clone()
            };
            prepared.insert(
                state.id,
                Decoration {
                    generation: state.generation,
                    clip: state.clip,
                    container: match old {
                        Some(l) => l.container.clone(),
                        None => self._compositor.CreateContainerVisual()?,
                    },
                    visual: match old {
                        Some(l) => l.visual.clone(),
                        None => self._compositor.CreateSpriteVisual()?,
                    },
                    surface,
                    commands: state.commands.clone(),
                    size: state.size,
                    dpi: self.dpi,
                    motions: old.map(|l| l.motions.clone()).unwrap_or_default(),
                },
            );
        }
        let now = Instant::now();
        let commit = (|| -> Result<()> {
            for state in &scene.layers {
                let layer = prepared.get_mut(&state.id).unwrap();
                let old = self.decorations.get(&state.id);
                // 裁剪放在不参与变换的父容器；子 SpriteVisual 独立移动/缩放。
                let container_size = self.decoration_root.Size()?;
                let clip_changed = old.is_none_or(|l| {
                    l.generation != state.generation || l.clip != state.clip || l.dpi != self.dpi
                }) || layer.container.Size()? != container_size;
                if clip_changed {
                    layer.container.SetSize(container_size)?;
                    if let Some(rect) = state.clip {
                        let clip = self._compositor.CreateInsetClip()?;
                        clip.SetLeftInset(rect.x * scale)?;
                        clip.SetTopInset(rect.y * scale)?;
                        clip.SetRightInset(container_size.x - (rect.x + rect.w) * scale)?;
                        clip.SetBottomInset(container_size.y - (rect.y + rect.h) * scale)?;
                        layer.container.SetClip(&clip)?;
                    } else {
                        layer
                            .container
                            .SetClip(None::<&Windows::UI::Composition::CompositionClip>)?;
                    }
                }
                if old.is_none_or(|l| l.generation != state.generation) {
                    layer.container.Children()?.InsertAtTop(&layer.visual)?;
                }
                if old.is_none_or(|l| l.surface != layer.surface) {
                    layer.visual.SetBrush(
                        &self
                            ._compositor
                            .CreateSurfaceBrushWithSurface(&layer.surface)?,
                    )?;
                    layer.visual.SetSize(Vector2 {
                        x: (state.size.0 * scale).ceil(),
                        y: (state.size.1 * scale).ceil(),
                    })?;
                }
                for motion in &state.motions {
                    let previous = layer.motions.get(&motion.property);
                    let repeated = previous.is_some_and(|p| {
                        p.revision == motion.revision && p.operation == motion.operation
                    });
                    let dpi_changed = old.is_some_and(|l| l.dpi != self.dpi);
                    if repeated && !dpi_changed {
                        continue;
                    }
                    let factor = match motion.property {
                        LayerProperty::OffsetX | LayerProperty::OffsetY => scale,
                        _ => 1.0,
                    };
                    let live = previous.is_some() && !dpi_changed && !motion.snap_from;
                    let name = property_name(motion.property);
                    match motion.operation {
                        LayerOperation::StopCurrent if live => layer.visual.StopAnimation(&name)?,
                        LayerOperation::Animate if motion.deadline > now => {
                            let animation = self._compositor.CreateScalarKeyFrameAnimation()?;
                            if live {
                                animation
                                    .InsertExpressionKeyFrame(0.0, &"this.StartingValue".into())?;
                            } else {
                                // 新建/恢复或显式 set 后的动画必须使用已知起点。
                                // 常量表达式和新动画在同批提交时，StartingValue 可能
                                // 仍是默认值，造成月球从零位置滑入，而非接续恢复高度。
                                animation.InsertKeyFrameWithEasingFunction(
                                    0.0,
                                    motion.value_at(now) * factor,
                                    &self._compositor.CreateLinearEasingFunction()?,
                                )?;
                            }
                            let easing: Windows::UI::Composition::CompositionEasingFunction =
                                match motion.easing {
                                    crate::abi::Easing::Linear => {
                                        self._compositor.CreateLinearEasingFunction()?.cast()?
                                    }
                                    other => {
                                        let (a, b) = match other {
                                            crate::abi::Easing::SmoothStep => (0.0, 1.0),
                                            crate::abi::Easing::EaseIn => (0.0, 1.0 / 3.0),
                                            _ => (2.0 / 3.0, 1.0),
                                        };
                                        self._compositor
                                            .CreateCubicBezierEasingFunction(
                                                Vector2 { x: 1.0 / 3.0, y: a },
                                                Vector2 { x: 2.0 / 3.0, y: b },
                                            )?
                                            .cast()?
                                    }
                                };
                            animation.InsertKeyFrameWithEasingFunction(
                                1.0,
                                motion.to * factor,
                                &easing,
                            )?;
                            let mut duration = animation.Duration()?;
                            duration.duration = (motion.deadline.duration_since(now).as_nanos()
                                / 100)
                                .max(10000) as i64;
                            animation.SetDuration(duration)?;
                            animation.SetStopBehavior(
                                Windows::UI::Composition::AnimationStopBehavior::LeaveCurrentValue,
                            )?;
                            layer.visual.StartAnimation(&name, &animation)?;
                        }
                        _ => set_value(&layer.visual, motion.property, motion.to * factor)?,
                    }
                    layer.motions.insert(motion.property, *motion);
                }
            }
            let order: Vec<_> = scene.ordered().iter().map(|l| l.id).collect();
            if order != self.decoration_order
                || prepared.iter().any(|(id, layer)| {
                    self.decorations
                        .get(id)
                        .is_none_or(|old| old.generation != layer.generation)
                })
            {
                let children = self.decoration_root.Children()?;
                children.RemoveAll()?;
                for id in &order {
                    children.InsertAtTop(&prepared[id].container)?;
                }
            }
            self.decoration_order = order;
            Ok(())
        })();
        if let Err(error) = commit {
            // A COM commit failure invalidates the cache; never claim the old
            // scene was successfully applied after a partial property update.
            let _ = self.clear_layers();
            return Err(error);
        }
        // Explicitly stop removed visuals, even if another COM reference exists.
        for (id, layer) in &self.decorations {
            if prepared
                .get(id)
                .is_none_or(|next| next.generation != layer.generation)
            {
                for property in layer.motions.keys() {
                    let _ = layer.visual.StopAnimation(&property_name(*property));
                }
            }
        }
        self.decorations = prepared;
        Ok(())
    }

    /// 结束 [`Self::begin_draw`] 开始的事务，并将表面提交给 Composition。
    ///
    /// 没有进行中的事务时返回 `E_UNEXPECTED`。即使底层 EndDraw 失败，事务守卫也已
    /// 消耗，不能再次结束同一事务。
    pub fn end_draw(&mut self) -> Result<()> {
        self.drawing
            .take()
            .ok_or_else(|| windows_core::Error::from_hresult(E_UNEXPECTED))?
            .finish()
    }

    #[cfg(test)]
    /// 返回测试断言所需层的视觉和表面引用；调用者取得的是额外 COM 引用。
    pub(crate) fn layer_objects(&self, id: u32) -> (SpriteVisual, CompositionDrawingSurface) {
        let layer = &self.decorations[&id];
        (layer.visual.clone(), layer.surface.clone())
    }

    /// 要求不处于 BeginDraw/EndDraw 之间；活动事务返回 `E_UNEXPECTED`。
    fn idle(&self) -> Result<()> {
        if self.drawing.is_some() {
            Err(windows_core::Error::from_hresult(E_UNEXPECTED))
        } else {
            Ok(())
        }
    }
}

/// 限制绘图表面的单边尺寸和总像素数，避免创建超大 GPU 资源。
///
/// 单边最大 16,384 像素，总量最大 16 Mi 像素；超限返回 `E_INVALIDARG`。零尺寸由此
/// 函数接受，具体调用方可另行要求正尺寸。
fn check_pixels(width: u32, height: u32) -> Result<()> {
    if width > 16_384 || height > 16_384 || u64::from(width) * u64::from(height) > 16 * 1024 * 1024
    {
        Err(windows_core::Error::from_hresult(E_INVALIDARG))
    } else {
        Ok(())
    }
}

impl Drop for Presenter {
    /// 若仍有绘制事务，先由守卫尽力结束；随后解除 HWND target 的根视觉。
    fn drop(&mut self) {
        drop(self.drawing.take());
        let _ = self.target.SetRoot(None);
    }
}
