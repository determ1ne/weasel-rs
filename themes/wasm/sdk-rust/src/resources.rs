//! 创建并复用宿主管理的字体、文本布局和图片资源。
//!
//! 公开资源值拥有一个不透明宿主句柄，不暴露 Win32 句柄或 WASM 线性内存指针；Rust
//! 所有者离开作用域时通过 `Drop` 释放句柄。宿主会继续保留已提交帧引用的资源，因此
//! 可以在帧提交后释放局部资源而不使旧画面悬空。资源创建可能因参数或配额失败，应检查 `Result`。
use crate::{raw, types::ResourceMetric};

/// 资源句柄的私有 RAII 所有者；各公开资源类型不可复制句柄。
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

/// 不可变字体资源。创建后可供多个文本布局复用。
pub struct Font(Handle);
impl Font {
    /// 创建字体；字号范围为 4–512 DIP，字重范围为 1–999（400 常规，700 粗体）。
    ///
    /// 返回的字体由 `Font` 所有；创建失败返回宿主负错误码。字体参数不应在每帧重复创建。
    pub fn new(family: &str, size: f32, weight: i32) -> Result<Self, i32> {
        Handle::new(unsafe { raw::font_create(family.as_ptr(), family.len() as i32, size, weight) })
            .map(Self)
    }
}
/// 由字体和文本生成的不可变排版结果，可同时用于测量和绘制。
pub struct TextLayout(Handle);
impl TextLayout {
    /// 创建文本布局；宽高采用 DIP，`wrap` 控制是否换行。
    ///
    /// 布局持有独立宿主句柄，创建后可跨帧复用；切换 DPI 无需重建 DIP 布局。文本和字体
    /// 必须在调用期间有效，宿主会同步复制所需内容。
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
    /// 返回布局宽度，单位为 DIP。
    pub fn width(&self) -> f32 {
        self.0.metric(ResourceMetric::Width)
    }
    /// 返回布局高度，单位为 DIP。
    pub fn height(&self) -> f32 {
        self.0.metric(ResourceMetric::Height)
    }
    /// 返回基线相对布局顶部的 DIP 距离。
    pub fn baseline(&self) -> f32 {
        self.0.metric(ResourceMetric::Baseline)
    }
    /// 将完整布局绘制到当前绘制目标，坐标为 DIP，颜色为非预乘 `0xAARRGGBB`。
    pub fn draw(&self, x: f32, y: f32, color: u32, glow: f32, glow_color: u32) {
        unsafe {
            raw::draw_layout(self.0.0, x, y, color, glow, glow_color);
        }
    }
}
/// 解码后的不可变图片资源；可跨帧重复绘制，不在每次绘制时重新读盘。
pub struct Image(Handle);
impl Image {
    /// 从模块旁的 `<模块名>.assets` 目录加载相对 PNG 路径。
    ///
    /// 绝对路径、父目录等路径形式不受支持；返回资源拥有自己的宿主句柄。
    pub fn load(name: &str) -> Result<Self, i32> {
        Handle::new(unsafe { raw::image_load(name.as_ptr(), name.len() as i32) }).map(Self)
    }
    /// 从内存中的 PNG 编码创建图片；宿主不会保留 `bytes` 指针。
    ///
    /// 编码上限为 8 MiB，解码尺寸上限为 4096；超过 `i32` 长度时返回资源限额错误。
    pub fn from_png(bytes: &[u8]) -> Result<Self, i32> {
        let len = i32::try_from(bytes.len()).map_err(|_| -6)?;
        Handle::new(unsafe { raw::image_create(bytes.as_ptr(), len) }).map(Self)
    }
    /// 返回解码后图片宽度，单位为 DIP。
    pub fn width(&self) -> f32 {
        self.0.metric(ResourceMetric::Width)
    }
    /// 返回解码后图片高度，单位为 DIP。
    pub fn height(&self) -> f32 {
        self.0.metric(ResourceMetric::Height)
    }
    /// 按目标 DIP 矩形绘制图片；透明度由宿主应用，范围错误按 ABI 合约处理。
    pub fn draw(&self, x: f32, y: f32, width: f32, height: f32, opacity: f32) {
        unsafe {
            raw::draw_image(self.0.0, x, y, width, height, opacity);
        }
    }
}
