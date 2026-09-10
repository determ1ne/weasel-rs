//! 只修改用户覆盖，保留未知字段；保存前检查外部修改。
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use weasel_common::{runtime_paths::RuntimePaths, settings::merge};
pub struct Document {
    pub base: Value,
    pub patch: Value,
    path: PathBuf,
    original: Option<Vec<u8>>,
}
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(1048577)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1048576 {
        return Err("配置超过 1 MiB".into());
    }
    Ok(Some(bytes))
}
fn object(bytes: &[u8]) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("配置必须为 JSON 对象".into());
    }
    Ok(value)
}
impl Document {
    pub fn effective(&self) -> Value {
        let mut result = self.base.clone();
        merge(&mut result, self.patch.clone());
        result
    }
    pub fn load(paths: &RuntimePaths) -> Result<Self, String> {
        let mut base = object(include_bytes!("../../weasel.json"))?;
        if let Some(bytes) = read(&paths.executable_directory.join("weasel.json"))? {
            merge(&mut base, object(&bytes)?);
        }
        let path = paths.user_data.join("weasel.custom.json");
        let original = read(&path)?;
        let patch = match &original {
            Some(bytes) => object(bytes)?,
            None => serde_json::json!({}),
        };
        Ok(Self {
            base,
            patch,
            path,
            original,
        })
    }
    pub fn save(&mut self) -> Result<(), String> {
        let parent = self.path.parent().ok_or("配置路径无父目录")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut bytes = serde_json::to_vec_pretty(&self.patch).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > 1048576 {
            return Err("配置超过 1 MiB".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        if read(&self.path)? != self.original {
            return Err("文件已被其他程序修改，请关闭后重新打开设置。".into());
        }
        file.persist(&self.path).map_err(|e| e.to_string())?;
        self.original = Some(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_unknown_fields_and_rejects_external_changes() {
        let directory = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::for_directory(directory.path().to_path_buf()).unwrap();
        // 不使用当前用户目录：整个读写检查仅发生在临时目录。
        let paths = RuntimePaths {
            user_data: directory.path().join("user"),
            ..paths
        };
        fs::create_dir_all(&paths.user_data).unwrap();
        let path = paths.user_data.join("weasel.custom.json");
        fs::write(
            &path,
            br#"{"themeSettings":{"future":{"value":42}},"ascii_mode":true}"#,
        )
        .unwrap();
        let mut doc = Document::load(&paths).unwrap();
        doc.patch.as_object_mut().unwrap().remove("ascii_mode");
        doc.save().unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["themeSettings"]["future"]["value"], 42);
        assert!(saved.get("ascii_mode").is_none());
        fs::write(&path, b"{}").unwrap();
        assert!(doc.save().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{}");
    }
}
