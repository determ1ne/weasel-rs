//! 实例私有资源。句柄单调分配、不复用；帧持有强引用，release 不会破坏已提交画面。
//! 配额按资源实际存活时间计费，而非只数句柄表，防止 release 后用画面引用绕过限额。
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

pub const INVALID_ARGUMENT: i32 = ErrorCode::InvalidArgument as i32;
pub const INVALID_HANDLE: i32 = ErrorCode::InvalidHandle as i32;
pub const RESOURCE_LIMIT: i32 = ErrorCode::ResourceLimit as i32;
pub const INTERNAL_ERROR: i32 = ErrorCode::Internal as i32;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENCODED: usize = 8 * 1024 * 1024;
const MAX_COUNT: usize = 512;

#[derive(Debug)]
pub enum Kind {
    Font(IDWriteTextFormat),
    Layout {
        layout: IDWriteTextLayout,
        metrics: [f32; 3],
        _font: Arc<Resource>,
    },
    Image {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    },
}
#[derive(Debug)]
pub struct Resource {
    pub id: i32,
    pub kind: Kind,
    bytes: usize,
    budget: Rc<(Cell<usize>, Cell<usize>)>,
}
impl PartialEq for Resource {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.budget.0.set(self.budget.0.get() - self.bytes);
        self.budget.1.set(self.budget.1.get() - 1);
    }
}
#[derive(Default)]
pub struct Resources {
    table: HashMap<i32, Arc<Resource>>,
    next: i32,
    budget: Rc<(Cell<usize>, Cell<usize>)>,
    write: Option<IDWriteFactory>,
    pub asset_root: Option<PathBuf>,
}
impl Resources {
    fn factory(&mut self) -> Result<IDWriteFactory, i32> {
        if let Some(factory) = &self.write {
            return Ok(factory.clone());
        }
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|_| INTERNAL_ERROR)?;
        self.write = Some(factory.clone());
        Ok(factory)
    }
    fn capacity(&self, bytes: usize) -> Result<(), i32> {
        if bytes > MAX_BYTES.saturating_sub(self.budget.0.get()) || self.budget.1.get() >= MAX_COUNT
        {
            Err(RESOURCE_LIMIT)
        } else {
            Ok(())
        }
    }
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
    pub fn get(&self, handle: i32) -> Result<Arc<Resource>, i32> {
        self.table.get(&handle).cloned().ok_or(INVALID_HANDLE)
    }
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
