//! 加载并合并由 broker 管理的启动配置。
//!
//! 配置文件按顺序叠加：对象会递归合并，数组和标量会替换已有值；单个文件无效时保留
//! 该文件应用前的配置。这里只检查文件、JSON 和传输大小；具体选项由消费组件校验。
use serde_json::Value;
use std::{
    io::{self, Read},
    path::Path,
};
use weasel_common::{
    process::RuntimePaths,
    settings::{ConfigSnapshot, merge},
};

/// 单个覆盖配置文件允许读取的最大字节数（1 MiB）。
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// 从打包默认值开始，依次应用可选的程序目录配置和用户自定义配置。
///
/// 后加载的文件优先级更高。缺失文件会被忽略；读取或解析失败时调用 `warn` 报告问题，
/// 并继续使用此前有效的值。
pub fn load(paths: &RuntimePaths, mut warn: impl FnMut(String)) -> ConfigSnapshot {
    // Also keep packaged defaults available to standalone cargo builds.
    let mut value: Value = serde_json::from_str(include_str!("../../weasel.json"))
        .expect("packaged settings must be valid JSON");
    for path in [
        paths.executable_directory.join("weasel.json"),
        paths.user_data.join("weasel.custom.json"),
    ] {
        if let Err(error) = overlay_file(&mut value, &path) {
            warn(format!(
                "settings {}: {error}; keeping previous settings",
                path.display()
            ));
        }
    }
    ConfigSnapshot::new(value)
}

/// 将单个可选配置文件叠加到基值；缺失文件不改变基值。
///
/// 单文件最多读取 1 MiB。文件读取、字节解码、JSON 解析、根对象检查和配置合并均在
/// 此处完成。新配置只有在全部检查通过后才会原子地替换基值。
fn overlay_file(base: &mut Value, path: &Path) -> Result<(), String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("configuration exceeds 1 MiB".into());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let patch: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if !patch.is_object() {
        return Err("configuration must be a JSON object".into());
    }
    let mut next = base.clone();
    merge(&mut next, patch);

    // 此处不理解 `theme`、`inline_preedit` 等业务选项。消费配置的组件负责验证
    // 自己使用的字段、报告错误并选择回退值。这里只保留 RPC 帧大小这一传输约束。
    // Leave room for protobuf fields when two individually valid files merge.
    if next.to_string().len() > weasel_common::data_frame::MAX_FRAME_SIZE - 4096 {
        return Err("merged configuration exceeds RPC size limit".into());
    }
    *base = next;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/settings.rs"]
mod tests;
