//! UI-thread GPU presentation. Window dimensions are pixels; panel dimensions are DIPs.
//! The returned render target is valid only until `end_draw`; do not call its
//! BeginDraw/EndDraw or replace its atlas-offset transform.
use crate::d2d_bindings::{Windows, *};
use Windows::Foundation::Size;
use Windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use Windows::System::{DispatcherQueue, DispatcherQueueController};
use Windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, Compositor, ContainerVisual,
    Desktop::DesktopWindowTarget, DropShadow, SpriteVisual,
};
use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use windows_core::{Interface, Result};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self> {
        // S_FALSE also increments the apartment's initialization count.
        unsafe {
            RoInitialize(RO_INIT_SINGLETHREADED).ok()?;
        }
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

struct OwnedQueue {
    controller: DispatcherQueueController,
    // Released after the controller, including on thread teardown.
    _apartment: Apartment,
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

fn ensure_queue() -> Result<()> {
    if DispatcherQueue::GetForCurrentThread().is_ok() {
        return Ok(());
    }
    let apartment = Apartment::new()?;
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

struct SurfaceDraw {
    interop: ICompositionDrawingSurfaceInterop,
    ended: bool,
}
impl SurfaceDraw {
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
    fn finish(mut self) -> Result<()> {
        self.ended = true;
        let result = unsafe { self.interop.EndDraw().ok() };
        // EndDraw must not be retried, even when it reports a lost device.
        result
    }
}
impl Drop for SurfaceDraw {
    fn drop(&mut self) {
        if !self.ended {
            unsafe {
                let _ = self.interop.EndDraw();
            }
        }
    }
}

pub struct Presenter {
    drawing: Option<SurfaceDraw>,
    target: DesktopWindowTarget,
    root: ContainerVisual,
    content: SpriteVisual,
    backdrop: SpriteVisual,
    backdrop_style: Option<crate::protocol::BackdropStyle>,
    hwnd: HWND,
    shadow_visual: SpriteVisual,
    shadow: DropShadow,
    surface: CompositionDrawingSurface,
    mask: CompositionDrawingSurface,
    _graphics: CompositionGraphicsDevice,
    _compositor: Compositor,
    _d2d: ID2D1Device,
    _d3d: ID3D11Device,
    _factory: ID2D1Factory1,
    dpi: u32,
    surface_size: (u32, u32),
    panel: Option<(crate::protocol::PanelStyle, f32, f32, u32)>,
    _ui_thread: PhantomData<Rc<()>>,
    // Rust drops fields in declaration order: COM resources must go first.
    _apartment: Apartment,
}

impl Presenter {
    /// The caller owns the HWND and pumps its UI-thread message loop.
    pub fn new(hwnd: HWND, dpi: u32, width: u32, height: u32) -> Result<Self> {
        check_pixels(width, height)?;
        let apartment = Apartment::new()?;
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
        target.SetRoot(&root)?;
        let mut presenter = Self {
            drawing: None,
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
        self.content.SetOffset(offset)?;
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
            let radius = style
                .corner_radius
                .max(0.0)
                .min(width / 2.0)
                .min(height / 2.0);
            target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width,
                        bottom: height,
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

    pub fn begin_draw(&mut self) -> Result<ID2D1RenderTarget> {
        self.idle()?;
        let (draw, target) = SurfaceDraw::begin(&self.surface, self.dpi)?;
        self.drawing = Some(draw);
        Ok(target)
    }

    pub fn end_draw(&mut self) -> Result<()> {
        self.drawing
            .take()
            .ok_or_else(|| windows_core::Error::from_hresult(E_UNEXPECTED))?
            .finish()
    }

    fn idle(&self) -> Result<()> {
        if self.drawing.is_some() {
            Err(windows_core::Error::from_hresult(E_UNEXPECTED))
        } else {
            Ok(())
        }
    }
}

fn check_pixels(width: u32, height: u32) -> Result<()> {
    if width > 16_384 || height > 16_384 || u64::from(width) * u64::from(height) > 16 * 1024 * 1024
    {
        Err(windows_core::Error::from_hresult(E_INVALIDARG))
    } else {
        Ok(())
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        drop(self.drawing.take());
        let _ = self.target.SetRoot(None);
    }
}
