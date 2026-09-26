//! 构造供 `Windows.UI.Composition` 消费的原生 D2D 效果描述，不依赖 Win2D。
//!
//! 呈现器负责启用 `DWMWA_USE_HOSTBACKDROPBRUSH` 并裁剪返回的画刷；本模块只组装效果树、
//! 绑定宿主背景画刷并提供禁用时的纯色回退。
use crate::d2d_bindings::{Windows, *};
use Windows::Foundation::{IPropertyValue, PropertyValue};
use Windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl,
};
use Windows::UI::Composition::{CompositionBrush, CompositionEffectSourceParameter, Compositor};
use std::sync::Mutex;
use windows_core::{GUID, HSTRING, Interface, PCWSTR, Result, implement};

/// 将渲染器诊断写入运行时日志；日志路径不可发现时退回标准错误。
///
/// 日志记录器通过进程级 `OnceLock` 延迟初始化并复用，消息只在本次调用中借用。
pub(crate) fn record(level: weasel_common::logging::Level, message: std::fmt::Arguments<'_>) {
    use std::sync::OnceLock;
    use weasel_common::{logging::ComponentLogger, process::RuntimePaths};
    static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
    LOGGER
        .get_or_init(|| match RuntimePaths::discover() {
            Ok(paths) => ComponentLogger::file_or_stderr(&paths.logs, "renderer").0,
            Err(_) => ComponentLogger::stderr(),
        })
        .record(level, "weasel-glass", message);
}

/// D2D 效果原生属性支持的 WinRT 值类型。
enum Property {
    /// 单个 32 位浮点属性。
    Float(f32),
    /// 32 位浮点数组属性，例如颜色矩阵或混合权重。
    Floats(Vec<f32>),
    /// 32 位无符号整数属性。
    Uint(u32),
    /// 布尔属性。
    Bool(bool),
}

/// 将 D2D 效果 ID、原生索引属性和输入节点暴露为 Composition 效果源。
///
/// 这些字段在效果创建后保持不变；只有 Composition 要求可设置的名称由互斥锁保护。
#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct Effect {
    /// D2D CLSID，决定原生效果类型。
    id: GUID,
    /// Composition 可读写的效果名称；锁中毒时接口方法返回 `E_UNEXPECTED`。
    name: Mutex<HSTRING>,
    /// 按 D2D 原生属性索引排列的参数。
    properties: Vec<Property>,
    /// 效果输入节点；强引用保证整棵效果树在绑定期间存活。
    sources: Vec<IGraphicsEffectSource>,
}

impl Effect {
    /// 创建仅作为效果源使用的 COM 对象，并固定其属性和输入节点。
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
    /// 返回当前效果名称的副本；锁中毒时返回 `E_UNEXPECTED`。
    fn Name(&self) -> Result<HSTRING> {
        Ok(self
            .name
            .lock()
            .map_err(|_| windows_core::Error::from_hresult(E_UNEXPECTED))?
            .clone())
    }
    /// 保存 Composition 设置的名称副本；锁中毒时返回 `E_UNEXPECTED`。
    fn SetName(&self, value: &HSTRING) -> Result<()> {
        *self
            .name
            .lock()
            .map_err(|_| windows_core::Error::from_hresult(E_UNEXPECTED))? = value.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for Effect_Impl {
    /// 返回该节点对应的原生 D2D 效果标识。
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(self.id)
    }
    /// 本实现不提供可动画命名属性；索引由 D2D 原生属性顺序直接指定。
    fn GetNamedPropertyMapping(&self, _: &PCWSTR, _: *mut u32, _: *mut i32) -> Result<()> {
        // Factory creation exposes no animatable properties; all indices below
        // are native D2D property indices, so no Win2D property mapping is needed.
        Err(windows_core::Error::from_hresult(E_INVALIDARG))
    }
    /// 返回原生效果属性数量。
    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(self.properties.len() as u32)
    }
    /// 按原生索引返回 WinRT 属性值；越界索引返回 `E_INVALIDARG`。
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
    /// 返回效果输入节点数。
    fn GetSourceCount(&self) -> Result<u32> {
        Ok(self.sources.len() as u32)
    }
    /// 按索引返回输入节点的强引用；越界索引返回 `E_INVALIDARG`。
    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        self.sources
            .get(index as usize)
            .cloned()
            .ok_or_else(|| windows_core::Error::from_hresult(E_INVALIDARG))
    }
}

/// 以 D2D ArithmeticComposite 将两个输入按给定系数合成。
///
/// 输出计算为 `wa * A + wb * B`，可按 `clamp` 控制结果截取；本函数不归一化权重。
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

/// 创建加权玻璃效果画刷；样式关闭时返回指定纯色画刷。
///
/// 启用时要求模糊半径与三个混合系数均为有限值且位于各自允许范围，违反约束返回
/// `E_INVALIDARG`。效果树或 Composition 资源创建失败会原样传播，供呈现器采用回退画刷。
/// 返回画刷绑定宿主背景输入；其 GPU 资源由 Composition 管理。
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

/// 将效果树编译为 Composition 画刷，并将指定名称绑定到宿主背景画刷。
///
/// `root` 在编译期间保持强引用；COM/Composition 创建、绑定或接口转换失败均通过
/// `windows_core::Result` 传播。
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
