//! 原创几何月球，构建时生成直通 alpha PNG；运行时只解码一次。
//! 不下载素材、不读取用户文件，也不把 PNG 编码器链接进 WASM。
use std::{fs::File, path::PathBuf};
#[path = "../../../build_support/wasm_theme_metadata.rs"]
mod metadata;

fn main() {
    metadata::generate();
    println!("cargo:rerun-if-changed=../../../build_support/wasm_theme_metadata.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let size = 192u32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let nx = (x as f32 + 0.5 - 96.0) / 82.0;
            let ny = (y as f32 + 0.5 - 96.0) / 82.0;
            let distance = nx.hypot(ny);
            let alpha = ((1.0 - distance) * 82.0 + 0.5).clamp(0.0, 1.0);
            let z = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
            let light = (-0.38 * nx - 0.45 * ny + 0.8 * z).max(0.0);
            let mut shade = 0.65 + 0.35 * light;
            for (cx, cy, radius) in [
                (-0.33, -0.18, 0.22),
                (0.3, 0.36, 0.15),
                (0.23, -0.43, 0.11),
                (-0.25, 0.46, 0.1),
            ] {
                let r = (nx - cx).hypot(ny - cy) / radius;
                shade -= 0.10 * (1.0 - r).clamp(0.0, 1.0);
                shade += 0.035 * (1.0 - (r - 1.0).abs() * 8.0).max(0.0);
            }
            pixels.extend([
                (246.0 * shade) as u8,
                (234.0 * shade) as u8,
                (207.0 * shade) as u8,
                (alpha * 255.0) as u8,
            ]);
        }
    }
    let path = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("moon.png");
    let mut encoder = png::Encoder::new(File::create(path).unwrap(), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&pixels)
        .unwrap();
}
