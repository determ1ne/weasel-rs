//! 可复用的宿主资源；不包含Win32句柄或guest指针。
//! Rust通过Drop释放句柄，host让已提交帧继续保有资源，避免悬空引用。
use crate::{raw, types::ResourceMetric};

struct Handle(i32);
impl Handle {
    fn new(id: i32) -> Result<Self, i32> {
        if id > 0 { Ok(Self(id)) } else { Err(id) }
    }
    fn metric(&self, field: ResourceMetric) -> f32 {
        unsafe { raw::resource_metric(self.0, field as i32) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            raw::resource_release(self.0);
        }
    }
}

pub struct Font(Handle);
impl Font {
    /// 字号DIP 4..512，字重1..999（400常规，700粗体）。字体对象不可变。
    pub fn new(family: &str, size: f32, weight: i32) -> Result<Self, i32> {
        Handle::new(unsafe { raw::font_create(family.as_ptr(), family.len() as i32, size, weight) })
            .map(Self)
    }
}
pub struct TextLayout(Handle);
impl TextLayout {
    /// 一次排版，同时用于测量和后续绘制。切换DPI无需重建DIP布局。
    pub fn new(font: &Font, text: &str, width: f32, height: f32, wrap: bool) -> Result<Self, i32> {
        Handle::new(unsafe {
            raw::text_layout_create(
                font.0.0,
                text.as_ptr(),
                text.len() as i32,
                width,
                height,
                wrap as i32,
            )
        })
        .map(Self)
    }
    pub fn width(&self) -> f32 {
        self.0.metric(ResourceMetric::Width)
    }
    pub fn height(&self) -> f32 {
        self.0.metric(ResourceMetric::Height)
    }
    pub fn baseline(&self) -> f32 {
        self.0.metric(ResourceMetric::Baseline)
    }
    pub fn draw(&self, x: f32, y: f32, color: u32, glow: f32, glow_color: u32) {
        unsafe {
            raw::draw_layout(self.0.0, x, y, color, glow, glow_color);
        }
    }
}
pub struct Image(Handle);
impl Image {
    /// 名称相对于 module.assets；加载一次后反复绘制，不在每帧读盘。
    pub fn load(name: &str) -> Result<Self, i32> {
        Handle::new(unsafe { raw::image_load(name.as_ptr(), name.len() as i32) }).map(Self)
    }
    pub fn from_png(bytes: &[u8]) -> Result<Self, i32> {
        let len = i32::try_from(bytes.len()).map_err(|_| -6)?;
        Handle::new(unsafe { raw::image_create(bytes.as_ptr(), len) }).map(Self)
    }
    pub fn width(&self) -> f32 {
        self.0.metric(ResourceMetric::Width)
    }
    pub fn height(&self) -> f32 {
        self.0.metric(ResourceMetric::Height)
    }
    pub fn draw(&self, x: f32, y: f32, width: f32, height: f32, opacity: f32) {
        unsafe {
            raw::draw_image(self.0.0, x, y, width, height, opacity);
        }
    }
}
