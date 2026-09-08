//! WebAssembly guest SDK. No JSON parser, WASI or native Windows dependencies.
pub const ABI_VERSION: i32 = 1;
/// Bold variant of the primary text font (slot 0), usable for measurement and drawing.
pub const FONT_TEXT_BOLD: i32 = 4;
pub const CANDIDATE: Data = Data(0);
pub const OPTIONS: Data = Data(1);
/// Host-provided global presentation settings (currently preedit_type).
pub const SETTINGS: Data = Data(2);

/// Persistent glass material. Sigma is DIP, weights sum to one, colors are ARGB.
/// The native host owns background sampling, effects and opaque fallback.
pub struct BackdropStyle {
    pub enabled: bool,
    pub tint: u32,
    pub blur_sigma: f32,
    pub backdrop_balance: f32,
    pub afterglow_balance: f32,
    pub color_balance: f32,
    pub fallback_color: u32,
}

pub fn set_backdrop(style: &BackdropStyle) {
    unsafe {
        raw::set_backdrop(
            style.enabled as i32,
            style.tint as i32,
            style.blur_sigma,
            style.backdrop_balance,
            style.afterglow_balance,
            style.color_balance,
            style.fallback_color as i32,
        );
    }
}

pub mod raw {
    #[link(wasm_import_module = "weasel")]
    unsafe extern "C" {
        pub fn set_text_glow(radius: f32, color: u32);
        pub fn data_kind(scope: i32, path: *const u8, len: i32) -> i32;
        pub fn data_len(scope: i32, path: *const u8, len: i32) -> i32;
        pub fn data_i64(scope: i32, path: *const u8, len: i32) -> i64;
        pub fn data_number(scope: i32, path: *const u8, len: i32) -> f64;
        pub fn data_string(
            scope: i32,
            path: *const u8,
            len: i32,
            dst: *mut u8,
            capacity: i32,
        ) -> i32;
        pub fn set_font(slot: i32, ptr: *const u8, len: i32);
        pub fn line_height(slot: i32, size: f32) -> f32;
        pub fn measure_text(ptr: *const u8, len: i32, font: i32, size: f32) -> f32;
        pub fn draw_text(
            ptr: *const u8,
            len: i32,
            x: f32,
            y: f32,
            font: i32,
            size: f32,
            color: u32,
        );
        pub fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32);
        pub fn fill_rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32);
        pub fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32);
        pub fn set_corner_radius(radius: f32);
        pub fn set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: i32);
        pub fn set_backdrop(
            enabled: i32,
            tint: i32,
            blur_sigma: f32,
            backdrop_balance: f32,
            afterglow_balance: f32,
            color_balance: f32,
            fallback_color: i32,
        );
        pub fn set_size(w: f32, h: f32);
        pub fn send_action(action: i32, index: i32);
        pub fn request_frame();
        pub fn time_ms() -> f64;
        pub fn log(ptr: *const u8, len: i32);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Missing,
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Copy)]
pub struct Data(i32);
impl Data {
    pub fn kind(self, path: &str) -> Kind {
        if path.len() > 1024 {
            return Kind::Missing;
        }
        match unsafe { raw::data_kind(self.0, path.as_ptr(), path.len() as i32) } {
            1 => Kind::Null,
            2 => Kind::Bool,
            3 => Kind::Number,
            4 => Kind::String,
            5 => Kind::Array,
            6 => Kind::Object,
            _ => Kind::Missing,
        }
    }
    pub fn len(self, path: &str) -> Option<usize> {
        if path.len() > 1024 {
            return None;
        }
        usize::try_from(unsafe { raw::data_len(self.0, path.as_ptr(), path.len() as i32) }).ok()
    }
    /// Integer values preserve all bits; cast to u64 for content_id.
    pub fn integer(self, path: &str) -> Option<i64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0, path.as_ptr(), path.len() as i32) })
    }
    pub fn boolean(self, path: &str) -> Option<bool> {
        if self.kind(path) != Kind::Bool {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0, path.as_ptr(), path.len() as i32) != 0 })
    }
    pub fn number(self, path: &str) -> Option<f64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_number(self.0, path.as_ptr(), path.len() as i32) })
    }
    pub fn string(self, path: &str) -> Option<String> {
        if self.kind(path) != Kind::String {
            return None;
        }
        let len = self.len(path)?;
        if len > 1024 * 1024 {
            return None;
        }
        let mut bytes = vec![0; len];
        let copied = unsafe {
            raw::data_string(
                self.0,
                path.as_ptr(),
                path.len() as i32,
                bytes.as_mut_ptr(),
                len as i32,
            )
        };
        if copied != len as i32 {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}

pub fn measure(text: &str, font: i32, size: f32) -> f32 {
    unsafe { raw::measure_text(text.as_ptr(), text.len() as i32, font, size) }
}
/// Soft text halo, radius 0..4 DIP; zero disables it for subsequent text draws.
pub fn set_text_glow(radius: f32, color: u32) {
    unsafe {
        raw::set_text_glow(radius, color);
    }
}
pub fn draw(text: &str, x: f32, y: f32, font: i32, size: f32, color: u32) {
    unsafe {
        raw::draw_text(text.as_ptr(), text.len() as i32, x, y, font, size, color);
    }
}
pub fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32) {
    unsafe {
        raw::fill_rect(x, y, w, h, color);
    }
}

/// Fill a single native rounded rectangle, with radius clamped to its bounds.
pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32) {
    unsafe {
        raw::fill_rounded_rect(x, y, w, h, radius, color);
    }
}
pub fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32) {
    unsafe {
        raw::stroke_rect(x, y, w, h, color, width);
    }
}
pub fn set_size(w: f32, h: f32) {
    unsafe {
        raw::set_size(w, h);
    }
}
/// Native panel: finite DIP corner radius 0..=4096, shadow radius 0..=250,
/// offsets -1024..=1024, ARGB color. These are host contract bounds.
/// Zero shadow radius disables shadow.
pub fn set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: u32) {
    unsafe { raw::set_panel(radius, shadow_radius, offset_x, offset_y, color as i32) }
}
pub fn send_action(action: i32, index: i32) {
    unsafe {
        raw::send_action(action, index);
    }
}
pub fn request_frame() {
    unsafe {
        raw::request_frame();
    }
}
pub fn time_ms() -> f64 {
    unsafe { raw::time_ms() }
}
pub fn log(message: &str) {
    unsafe {
        raw::log(message.as_ptr(), message.len() as i32);
    }
}
