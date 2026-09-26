//! 管理单个主题实例创建的字体、文本布局和图像资源，并注册对应的 guest 导入函数。
//!
//! 句柄按实例单调递增且不复用。绘制命令持有资源的强引用，因此释放句柄只会阻止后续
//! 查询和提交，不会使已经提交的帧失效；字节数和资源数配额则一直计到最后一个强引用
//! 被丢弃，避免借助在途帧绕过限额。
use crate::abi::{ErrorCode, ResourceMetric};
use crate::{
    d2d_bindings::*,
    protocol::IMPORT_MODULE,
    runtime::{HostState, read_wasm_string},
};
use std::{
    cell::Cell,
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};
use wasmtime::{Caller, Linker};
use windows_core::Interface;
use windows_strings::{HSTRING, w};

/// ABI 返回码：调用参数或资源内容不合法。
pub const INVALID_ARGUMENT: i32 = ErrorCode::InvalidArgument as i32;
/// ABI 返回码：句柄不存在、已释放或资源类型不匹配。
pub const INVALID_HANDLE: i32 = ErrorCode::InvalidHandle as i32;
/// ABI 返回码：实例资源配额耗尽或句柄编号溢出。
pub const RESOURCE_LIMIT: i32 = ErrorCode::ResourceLimit as i32;
/// ABI 返回码：原生资源创建或内部操作失败。
pub const INTERNAL_ERROR: i32 = ErrorCode::Internal as i32;
/// 单个实例可同时持有的资源估算字节数上限。
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// 单次编码图像输入的最大字节数。
const MAX_ENCODED: usize = 8 * 1024 * 1024;
/// 单个实例可同时持有的资源数量上限。
const MAX_COUNT: usize = 512;

/// 资源实际内容；变体决定该句柄可用于哪些查询和绘制操作。
#[derive(Debug)]
pub enum Kind {
    /// DirectWrite 字体格式对象。
    Font(IDWriteTextFormat),
    /// DirectWrite 文本布局及预先计算的宽、高和首行基线。
    Layout {
        /// 原生布局对象，由拥有它的资源保持存活。
        layout: IDWriteTextLayout,
        /// 依次为包含尾随空白的宽度、布局高度和首行基线，单位为 DIP。
        metrics: [f32; 3],
        /// 创建此布局所用的字体；此强引用保证字体至少与布局同寿命。
        _font: Arc<Resource>,
    },
    /// 解码后的 BGRA 预乘 Alpha 像素。
    Image {
        /// 每像素四字节，顺序为 B、G、R、A，RGB 已按 Alpha 预乘。
        pixels: Vec<u8>,
        /// 解码后的像素宽度。
        width: u32,
        /// 解码后的像素高度。
        height: u32,
    },
}
/// 具备共享所有权的实例资源。
///
/// 资源由句柄表及已提交的绘制命令共同持有。最后一个 `Arc` 被释放时才返还记账配额。
/// 相等性按对象身份判定。内部配额使用 `Rc<Cell<_>>`，因此资源属于实例所在的线程，
/// 不可跨线程共享。
#[derive(Debug)]
pub struct Resource {
    /// 此资源在所属实例中的句柄；句柄不会复用。
    pub id: i32,
    /// 资源类型及其原生对象或像素数据。
    pub kind: Kind,
    /// 该资源向实例字节配额计入的估算大小。
    bytes: usize,
    /// 所属 `Resources` 的共享配额计数；资源释放时递减。
    budget: Rc<(Cell<usize>, Cell<usize>)>,
}
impl PartialEq for Resource {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Drop for Resource {
    /// 最后一个所有者释放资源时返还字节数和资源数配额。
    fn drop(&mut self) {
        self.budget.0.set(self.budget.0.get() - self.bytes);
        self.budget.1.set(self.budget.1.get() - 1);
    }
}
#[derive(Default)]
/// 单个主题实例的资源表、分配器和资源配额。
///
/// 状态由实例所属线程独占访问；配额计数不是原子操作，不能并发修改。
pub struct Resources {
    /// 当前可由 guest 句柄访问的资源；移除表项不会使其他强引用失效。
    table: HashMap<i32, Arc<Resource>>,
    /// 最近分配的句柄编号；从零开始递增且溢出时拒绝分配。
    next: i32,
    /// 所有存活资源共同更新的（字节数，资源数）计数。
    budget: Rc<(Cell<usize>, Cell<usize>)>,
    /// 延迟创建并在本实例内复用的 DirectWrite 工厂。
    write: Option<IDWriteFactory>,
    /// 只读资源文件的根目录；加载时仍会规范化并检查路径边界。
    pub asset_root: Option<PathBuf>,
}
impl Resources {
    /// 获取共享 DirectWrite 工厂，首次调用时创建并缓存。
    fn factory(&mut self) -> Result<IDWriteFactory, i32> {
        if let Some(factory) = &self.write {
            return Ok(factory.clone());
        }
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|_| INTERNAL_ERROR)?;
        self.write = Some(factory.clone());
        Ok(factory)
    }
    /// 检查新增资源是否会突破存活资源的字节数或数量上限。
    fn capacity(&self, bytes: usize) -> Result<(), i32> {
        if bytes > MAX_BYTES.saturating_sub(self.budget.0.get()) || self.budget.1.get() >= MAX_COUNT
        {
            Err(RESOURCE_LIMIT)
        } else {
            Ok(())
        }
    }
    /// 分配新句柄并记账；容量不足或句柄溢出时不插入资源。
    fn insert(&mut self, kind: Kind, bytes: usize) -> Result<i32, i32> {
        self.capacity(bytes)?;
        self.next = self.next.checked_add(1).ok_or(RESOURCE_LIMIT)?;
        self.budget.0.set(self.budget.0.get() + bytes);
        self.budget.1.set(self.budget.1.get() + 1);
        self.table.insert(
            self.next,
            Arc::new(Resource {
                id: self.next,
                kind,
                bytes,
                budget: self.budget.clone(),
            }),
        );
        Ok(self.next)
    }
    /// 按句柄取得强引用；句柄已释放或不存在时返回 `INVALID_HANDLE`。
    pub fn get(&self, handle: i32) -> Result<Arc<Resource>, i32> {
        self.table.get(&handle).cloned().ok_or(INVALID_HANDLE)
    }
    /// 创建无换行字体格式。
    ///
    /// 字体名、字号和字重先经过边界校验；输入不合法返回 `INVALID_ARGUMENT`，
    /// DirectWrite 创建失败返回 `INTERNAL_ERROR`，配额不足返回 `RESOURCE_LIMIT`。
    pub(crate) fn font(&mut self, name: &str, size: f32, weight: i32) -> Result<i32, i32> {
        if name.is_empty()
            || name.len() > 512
            || !size.is_finite()
            || !(4.0..=512.0).contains(&size)
            || !(1..=999).contains(&weight)
        {
            return Err(INVALID_ARGUMENT);
        }
        self.capacity(4096)?;
        let name = HSTRING::from(name);
        let format = unsafe {
            self.factory()?.CreateTextFormat(
                &name,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("zh-CN"),
            )
        }
        .map_err(|_| INTERNAL_ERROR)?;
        unsafe { format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok() }
            .map_err(|_| INTERNAL_ERROR)?;
        self.insert(Kind::Font(format), 4096)
    }
    /// 按已有字体和 guest 文本创建布局并缓存布局度量。
    ///
    /// 尺寸必须是有限且大于零的 DIP，换行标志只能为 0 或 1。字体句柄必须指向
    /// `Font`；布局会强持有该字体。非法参数、句柄、配额和 DirectWrite 错误分别映射为
    /// `INVALID_ARGUMENT`、`INVALID_HANDLE`、`RESOURCE_LIMIT` 和 `INTERNAL_ERROR`。
    pub(crate) fn layout(
        &mut self,
        font: i32,
        text: &str,
        width: f32,
        height: f32,
        wrap: i32,
    ) -> Result<i32, i32> {
        if ![width, height]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && *v <= 65536.0)
            || !matches!(wrap, 0 | 1)
        {
            return Err(INVALID_ARGUMENT);
        }
        let charge = 4096 + text.len() * 32;
        self.capacity(charge)?;
        let font = self.get(font)?;
        let Kind::Font(format) = &font.kind else {
            return Err(INVALID_HANDLE);
        };
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            self.factory()?
                .CreateTextLayout(&utf16, format, width, height)
        }
        .map_err(|_| INTERNAL_ERROR)?;
        let result = (|| -> windows_core::Result<[f32; 3]> {
            unsafe {
                layout
                    .cast::<IDWriteTextFormat>()?
                    .SetWordWrapping(if wrap != 0 {
                        DWRITE_WORD_WRAPPING_WRAP
                    } else {
                        DWRITE_WORD_WRAPPING_NO_WRAP
                    })
                    .ok()?;
                let mut metrics = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut metrics).ok()?;
                let mut count = 0;
                let _ = layout.GetLineMetrics(None, 0, &mut count);
                if count > 65536 {
                    return Err(windows_core::Error::from_hresult(E_INVALIDARG));
                }
                let mut lines = vec![DWRITE_LINE_METRICS::default(); count as usize];
                if count != 0 {
                    layout
                        .GetLineMetrics(Some(lines.as_mut_ptr()), count, &mut count)
                        .ok()?;
                }
                Ok([
                    metrics.widthIncludingTrailingWhitespace,
                    metrics.height,
                    lines.first().map_or(0.0, |v| v.baseline),
                ])
            }
        })();
        self.insert(
            Kind::Layout {
                layout,
                metrics: result.map_err(|_| INTERNAL_ERROR)?,
                _font: font,
            },
            charge,
        )
    }
    /// 解码静态 PNG，并转换为 BGRA 预乘 Alpha 格式后记入资源表。
    ///
    /// 拒绝动画、零尺寸及超过边界的图像；编码/格式错误返回 `INVALID_ARGUMENT`，
    /// 尺寸或配额超限返回 `RESOURCE_LIMIT`。
    pub(crate) fn image(&mut self, bytes: &[u8]) -> Result<i32, i32> {
        if bytes.len() > MAX_ENCODED {
            return Err(RESOURCE_LIMIT);
        }
        let mut decoder =
            png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: MAX_BYTES });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|_| INVALID_ARGUMENT)?;
        let (w, h) = (reader.info().width, reader.info().height);
        if w == 0 || h == 0 || w > 4096 || h > 4096 || reader.info().animation_control.is_some() {
            return Err(INVALID_ARGUMENT);
        }
        let size = (w as usize)
            .checked_mul(h as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or(RESOURCE_LIMIT)?;
        self.capacity(size)?;
        let decoded = reader
            .output_buffer_size()
            .filter(|v| *v <= MAX_BYTES)
            .ok_or(RESOURCE_LIMIT)?;
        let mut buffer = vec![0u8; decoded];
        let output = reader
            .next_frame(&mut buffer)
            .map_err(|_| INVALID_ARGUMENT)?;
        let channels = match output.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            _ => return Err(INVALID_ARGUMENT),
        };
        let mut pixels = Vec::with_capacity(size);
        for p in buffer[..output.buffer_size()].chunks_exact(channels) {
            let (r, g, b, a) = match channels {
                4 => (p[0], p[1], p[2], p[3]),
                3 => (p[0], p[1], p[2], 255),
                2 => (p[0], p[0], p[0], p[1]),
                _ => (p[0], p[0], p[0], 255),
            };
            let pm = |v: u8| ((v as u16 * a as u16 + 127) / 255) as u8;
            pixels.extend_from_slice(&[pm(b), pm(g), pm(r), a]);
        }
        self.insert(
            Kind::Image {
                pixels,
                width: w,
                height: h,
            },
            size,
        )
    }
    /// 从实例资源根目录加载相对 PNG 路径。
    ///
    /// 仅接受以 `.png` 结尾且不含绝对路径、盘符、备用数据流或父目录段的名称；
    /// 规范化后还必须位于资源根目录内。文件读取/根目录不可用使用内部哨兵错误，
    /// 编码大小超限返回 `RESOURCE_LIMIT`，图像内容校验交由 [`Self::image`] 完成。
    fn load_image(&mut self, name: &str) -> Result<i32, i32> {
        // 仅允许 .wasm 同名 .assets 目录内的相对 PNG；拒绝绝对路径、ADS 和父目录。
        if name.is_empty()
            || name.len() > 1024
            || name.contains([':', '\\'])
            || name
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || !name.ends_with(".png")
        {
            return Err(INVALID_ARGUMENT);
        }
        let root = self
            .asset_root
            .as_ref()
            .ok_or(-1)?
            .canonicalize()
            .map_err(|_| -1)?;
        let path = root.join(name).canonicalize().map_err(|_| -1)?;
        if !path.starts_with(&root) {
            return Err(INVALID_ARGUMENT);
        }
        let file = std::fs::File::open(path).map_err(|_| -1)?;
        if file.metadata().map_err(|_| -1)?.len() > MAX_ENCODED as u64 {
            return Err(RESOURCE_LIMIT);
        }
        let mut bytes = Vec::new();
        file.take(MAX_ENCODED as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| INTERNAL_ERROR)?;
        self.image(&bytes)
    }
}

/// 将资源创建、查询、绘制和释放操作绑定到主题 ABI 导入命名空间。
///
/// 用户输入及资源配额错误通过 ABI 返回码或 Wasmtime 错误传递；注册失败由调用方处理。
/// 度量查询仅适用于布局和图像的支持字段；绘制会校验句柄类型及样式，释放只移除句柄表
/// 中的所有权，已有帧仍可通过其强引用完成回放。
pub fn register(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    linker.func_wrap(
        IMPORT_MODULE,
        "font_create",
        |mut c: Caller<'_, HostState>,
         p: i32,
         n: i32,
         size: f32,
         weight: i32|
         -> wasmtime::Result<i32> {
            c.data_mut().charge_resource(false)?;
            c.data_mut().charge(n.max(0) as usize)?;
            let Some(name) = read_wasm_string(&mut c, p, n) else {
                return Ok(INVALID_ARGUMENT);
            };
            Ok(c.data_mut()
                .resources
                .font(&name, size, weight)
                .unwrap_or_else(|e| e))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "text_layout_create",
        |mut c: Caller<'_, HostState>,
         font: i32,
         p: i32,
         n: i32,
         w: f32,
         h: f32,
         wrap: i32|
         -> wasmtime::Result<i32> {
            c.data_mut().charge_resource(false)?;
            c.data_mut().charge(n.max(0) as usize)?;
            let Some(text) = read_wasm_string(&mut c, p, n) else {
                return Ok(INVALID_ARGUMENT);
            };
            Ok(c.data_mut()
                .resources
                .layout(font, &text, w, h, wrap)
                .unwrap_or_else(|e| e))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "image_load",
        |mut c: Caller<'_, HostState>, p: i32, n: i32| -> wasmtime::Result<i32> {
            c.data_mut().charge_resource(true)?;
            c.data_mut().charge(n.max(0) as usize)?;
            let Some(name) = read_wasm_string(&mut c, p, n) else {
                return Ok(INVALID_ARGUMENT);
            };
            Ok(c.data_mut()
                .resources
                .load_image(&name)
                .unwrap_or_else(|e| e))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "image_create",
        |mut c: Caller<'_, HostState>, p: u32, n: u32| -> wasmtime::Result<i32> {
            c.data_mut().charge_resource(true)?;
            c.data_mut().charge(0)?;
            if n as usize > MAX_ENCODED {
                return Ok(RESOURCE_LIMIT);
            }
            let memory = c
                .get_export("memory")
                .and_then(|v| v.into_memory())
                .ok_or_else(|| wasmtime::format_err!("missing memory"))?;
            let Some(end) = (p as usize).checked_add(n as usize) else {
                return Ok(INVALID_ARGUMENT);
            };
            let Some(bytes) = memory.data(&c).get(p as usize..end) else {
                return Ok(INVALID_ARGUMENT);
            };
            let bytes = bytes.to_vec();
            Ok(c.data_mut().resources.image(&bytes).unwrap_or_else(|e| e))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "resource_release",
        |mut c: Caller<'_, HostState>, id: i32| -> wasmtime::Result<i32> {
            c.data_mut().charge(0)?;
            Ok(if c.data_mut().resources.table.remove(&id).is_some() {
                0
            } else {
                INVALID_HANDLE
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "resource_metric",
        |mut c: Caller<'_, HostState>, id: i32, metric: i32| -> wasmtime::Result<f32> {
            c.data_mut().charge(0)?;
            let resource = c
                .data()
                .resources
                .get(id)
                .map_err(|_| wasmtime::format_err!("invalid resource handle"))?;
            let metric = ResourceMetric::try_from(metric)
                .map_err(|_| wasmtime::format_err!("invalid resource metric"))?;
            match &resource.kind {
                Kind::Layout { metrics, .. } => Ok(metrics[metric as usize]),
                Kind::Image { width, height, .. }
                    if matches!(metric, ResourceMetric::Width | ResourceMetric::Height) =>
                {
                    Ok(if metric == ResourceMetric::Width {
                        *width as f32
                    } else {
                        *height as f32
                    })
                }
                _ => Err(wasmtime::format_err!("invalid resource metric")),
            }
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "draw_layout",
        |mut c: Caller<'_, HostState>,
         id: i32,
         x: f32,
         y: f32,
         color: u32,
         glow: f32,
         glow_color: u32|
         -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            let resource = c
                .data()
                .resources
                .get(id)
                .map_err(|_| wasmtime::format_err!("invalid layout handle"))?;
            if !matches!(resource.kind, Kind::Layout { .. })
                || !glow.is_finite()
                || !(0.0..=4.0).contains(&glow)
            {
                return Err(wasmtime::format_err!("invalid layout style"));
            }
            c.data_mut().draw(crate::protocol::DrawCommand::Layout {
                resource,
                x,
                y,
                color,
                glow: (glow, glow_color),
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "draw_image",
        |mut c: Caller<'_, HostState>,
         id: i32,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         opacity: f32|
         -> wasmtime::Result<()> {
            c.data_mut().charge(0)?;
            let resource = c
                .data()
                .resources
                .get(id)
                .map_err(|_| wasmtime::format_err!("invalid image handle"))?;
            if !matches!(resource.kind, Kind::Image { .. })
                || !opacity.is_finite()
                || !(0.0..=1.0).contains(&opacity)
                || w <= 0.0
                || h <= 0.0
            {
                return Err(wasmtime::format_err!("invalid image style"));
            }
            c.data_mut().draw(crate::protocol::DrawCommand::Image {
                resource,
                x,
                y,
                w,
                h,
                opacity,
            })
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_premultiplied_and_frame_owns_released_resources() {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[200, 100, 50, 128])
                .unwrap();
        }
        let mut resources = Resources::default();
        let first = resources.image(&encoded).unwrap();
        let frame_ref = resources.get(first).unwrap();
        let Kind::Image { pixels, .. } = &frame_ref.kind else {
            panic!("image expected")
        };
        assert_eq!(pixels, &[25, 50, 100, 128]);
        resources.table.remove(&first);
        assert!(resources.get(first).is_err());
        assert_eq!(resources.budget.0.get(), 4); // 帧仍然持有，不释放配额。
        drop(frame_ref);
        assert_eq!(resources.budget.0.get(), 0);
        assert!(resources.image(&encoded).unwrap() > first);
        assert_eq!(resources.image(b"invalid png"), Err(INVALID_ARGUMENT));
        for path in [
            "../a.png",
            "/a.png",
            "C:/a.png",
            "a//b.png",
            "a.png:stream",
            "a.webp",
        ] {
            assert_eq!(resources.load_image(path), Err(INVALID_ARGUMENT));
        }
    }
}
