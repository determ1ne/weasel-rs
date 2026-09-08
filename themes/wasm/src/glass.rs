//! Native D2D effect descriptions consumed by Windows.UI.Composition (no Win2D).
//! The presenter enables DWMWA_USE_HOSTBACKDROPBRUSH and clips the returned brush.
use crate::d2d_bindings::{Windows, *};
use Windows::Foundation::{IPropertyValue, PropertyValue};
use Windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl,
};
use Windows::UI::Composition::{CompositionBrush, CompositionEffectSourceParameter, Compositor};
use std::sync::Mutex;
use windows_core::{GUID, HSTRING, Interface, PCWSTR, Result, implement};

pub(crate) fn record(level: weasel_common::logging::Level, message: std::fmt::Arguments<'_>) {
    use std::sync::OnceLock;
    use weasel_common::{logging::ComponentLogger, runtime_paths::RuntimePaths};
    static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
    LOGGER
        .get_or_init(|| match RuntimePaths::discover() {
            Ok(paths) => ComponentLogger::file_or_stderr(&paths.logs, "renderer").0,
            Err(_) => ComponentLogger::stderr(),
        })
        .record(level, "weasel-glass", message);
}

enum Property {
    Float(f32),
    Floats(Vec<f32>),
    Uint(u32),
    Bool(bool),
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct Effect {
    id: GUID,
    name: Mutex<HSTRING>,
    properties: Vec<Property>,
    sources: Vec<IGraphicsEffectSource>,
}

impl Effect {
    fn source(
        id: GUID,
        properties: Vec<Property>,
        sources: Vec<IGraphicsEffectSource>,
    ) -> IGraphicsEffectSource {
        Self {
            id,
            name: Mutex::new(HSTRING::new()),
            properties,
            sources,
        }
        .into()
    }
}

impl IGraphicsEffectSource_Impl for Effect_Impl {}

impl IGraphicsEffect_Impl for Effect_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self
            .name
            .lock()
            .map_err(|_| windows_core::Error::from_hresult(E_UNEXPECTED))?
            .clone())
    }
    fn SetName(&self, value: &HSTRING) -> Result<()> {
        *self
            .name
            .lock()
            .map_err(|_| windows_core::Error::from_hresult(E_UNEXPECTED))? = value.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for Effect_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(self.id)
    }
    fn GetNamedPropertyMapping(&self, _: &PCWSTR, _: *mut u32, _: *mut i32) -> Result<()> {
        // Factory creation exposes no animatable properties; all indices below
        // are native D2D property indices, so no Win2D property mapping is needed.
        Err(windows_core::Error::from_hresult(E_INVALIDARG))
    }
    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(self.properties.len() as u32)
    }
    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        let value = match self.properties.get(index as usize) {
            Some(Property::Float(v)) => PropertyValue::CreateSingle(*v)?,
            Some(Property::Floats(v)) => PropertyValue::CreateSingleArray(v)?,
            Some(Property::Uint(v)) => PropertyValue::CreateUInt32(*v)?,
            Some(Property::Bool(v)) => PropertyValue::CreateBoolean(*v)?,
            None => return Err(windows_core::Error::from_hresult(E_INVALIDARG)),
        };
        value.cast()
    }
    fn GetSourceCount(&self) -> Result<u32> {
        Ok(self.sources.len() as u32)
    }
    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        self.sources
            .get(index as usize)
            .cloned()
            .ok_or_else(|| windows_core::Error::from_hresult(E_INVALIDARG))
    }
}

fn sum(
    a: IGraphicsEffectSource,
    b: IGraphicsEffectSource,
    wa: f32,
    wb: f32,
    clamp: bool,
) -> IGraphicsEffectSource {
    // ArithmeticComposite: k1*A*B + k2*A + k3*B + k4.
    Effect::source(
        CLSID_D2D1ArithmeticComposite,
        vec![
            Property::Floats(vec![0.0, wa, wb, 0.0]),
            Property::Bool(clamp),
        ],
        vec![a, b],
    )
}

/// Returns the weighted glass graph, or a solid fallback when disabled.
/// Enabled-path errors are propagated so the presenter can select its fallback.
pub(super) fn create_brush(
    compositor: &Compositor,
    style: &crate::protocol::BackdropStyle,
) -> Result<CompositionBrush> {
    if !style.enabled {
        let color = style.fallback_color;
        return compositor
            .CreateColorBrushWithColor(Windows::UI::Color {
                A: (color >> 24) as u8,
                R: (color >> 16) as u8,
                G: (color >> 8) as u8,
                B: color as u8,
            })?
            .cast();
    }
    if !style.blur_sigma.is_finite()
        || !(0.0..=250.0).contains(&style.blur_sigma)
        || [
            style.backdrop_balance,
            style.afterglow_balance,
            style.color_balance,
        ]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(windows_core::Error::from_hresult(E_INVALIDARG));
    }
    let source_name = HSTRING::from("HostBackdrop");
    // GaussianBlur properties: sigma, balanced optimization (1), hard border (1).
    // Composition requires a tree, not shared effect instances (a DAG). Each
    // branch has its own blur and source-parameter node; the name binds both
    // leaves to the same host backdrop brush.
    let make_blur = || -> Result<IGraphicsEffectSource> {
        let source = CompositionEffectSourceParameter::Create(&source_name)?.cast()?;
        Ok(Effect::source(
            CLSID_D2D1GaussianBlur,
            vec![
                Property::Float(style.blur_sigma),
                Property::Uint(1),
                Property::Uint(1),
            ],
            vec![source],
        ))
    };
    let blur = make_blur()?;
    let r = ((style.tint >> 16) & 255) as f32 / 255.0;
    let g = ((style.tint >> 8) & 255) as f32 / 255.0;
    let b = (style.tint & 255) as f32 / 255.0;
    // Row-major D2D 5x4 matrix: luminance (Rec.709) multiplied by tint RGB.
    // Preserve backdrop alpha; the constant tint branch honors tint alpha.
    let tinted = Effect::source(
        CLSID_D2D1ColorMatrix,
        vec![
            Property::Floats(vec![
                0.2126 * r,
                0.2126 * g,
                0.2126 * b,
                0.0,
                0.7152 * r,
                0.7152 * g,
                0.7152 * b,
                0.0,
                0.0722 * r,
                0.0722 * g,
                0.0722 * b,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ]),
            Property::Uint(1),
            Property::Bool(false),
        ],
        vec![make_blur()?],
    );
    let tint = Effect::source(
        CLSID_D2D1Flood,
        vec![Property::Floats(vec![
            r,
            g,
            b,
            (style.tint >> 24) as f32 / 255.0,
        ])],
        vec![],
    );
    // Default balances yield .49*blur + .43*tinted grayscale blur + .08*tint.
    // Do not normalize: the style supplies the actual arithmetic coefficients.
    let mixed = sum(
        blur,
        tinted,
        style.backdrop_balance,
        style.afterglow_balance,
        false,
    );
    bind(
        compositor,
        &source_name,
        sum(mixed, tint, 1.0, style.color_balance, true),
    )
}

fn bind(
    compositor: &Compositor,
    source_name: &HSTRING,
    root: IGraphicsEffectSource,
) -> Result<CompositionBrush> {
    let root: IGraphicsEffect = root.cast()?;
    let brush = compositor.CreateEffectFactory(&root)?.CreateBrush()?;
    brush.SetSourceParameter(source_name, &compositor.CreateHostBackdropBrush()?)?;
    brush.cast()
}
