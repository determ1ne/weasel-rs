//! 配置快照的本地解析、查询和默认值合并。
//!
//! [`ConfigSnapshot`] 持有不可变 JSON 配置，查询结果借用快照中的值；合并主题默认值
//! 会创建新快照，不修改原快照。配置从何处取得由各个消费组件决定。
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;

/// 可廉价克隆的不可变配置快照。
///
/// 内部 JSON 通过引用计数共享；查询不会修改配置。调用
/// [`with_theme_defaults`](Self::with_theme_defaults) 会返回独立快照，并保留原快照不变。
#[derive(Clone, Debug)]
pub struct ConfigSnapshot(Arc<Value>);

impl ConfigSnapshot {
    /// 将 JSON 值封装为共享的只读快照。
    pub fn new(value: Value) -> Self {
        Self(Arc::new(value))
    }
    /// 解析 JSON 根对象并构造配置快照。
    pub fn from_json(json: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if !value.is_object() {
            return Err("configuration root must be an object".into());
        }
        Ok(Self::new(value))
    }
    /// 按受限的 jq 风格路径查询配置值。
    ///
    /// 返回的引用与快照具有相同生命周期；路径不存在时返回 `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 路径语法无效或超过长度限制时返回描述错误的字符串。
    pub fn query(&self, path: &str) -> Result<Option<&Value>, String> {
        query(&self.0, path)
    }
    /// 查询并反序列化指定配置项。
    ///
    /// 路径不存在时返回 `Ok(None)`；存在但类型不匹配或值无法反序列化时返回错误。
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, String> {
        self.query(path)?
            .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
            .transpose()
    }
    /// 查询并反序列化必需配置项。
    ///
    /// # Errors
    ///
    /// 路径不存在、语法无效或值不能反序列化为目标类型时返回错误。
    pub fn required<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        self.get(path)?
            .ok_or_else(|| format!("missing required configuration value {path}"))
    }

    /// 查询应用布尔选项；应用未指定时继承同名全局选项。
    ///
    /// 应用名比较不区分 ASCII 大小写。配置合并阶段保证全局选项存在，因此缺失或
    /// 类型不正确表示配置快照违反内部不变量。
    pub fn app_bool(&self, executable: &str, option: &str) -> Result<bool, String> {
        let value = self
            .application_value(executable, option)?
            .or_else(|| self.0.get(option))
            .ok_or_else(|| format!("missing required configuration value .{option}"))?;
        value
            .as_bool()
            .ok_or_else(|| format!("configuration value {option} must be a boolean"))
    }

    /// 判断是否有任何应用需要外置 preedit 能力。
    ///
    /// 除全局设置外，也检查所有 `app_options` 覆盖；只要任一应用显式关闭
    /// `inline_preedit`，就必须提前协商该能力。
    pub fn needs_external_preedit(&self) -> Result<bool, String> {
        if !self.required::<bool>(".inline_preedit")? {
            return Ok(true);
        }
        let apps = self
            .0
            .get("app_options")
            .and_then(Value::as_object)
            .ok_or("app_options must be an object")?;
        for (name, options) in apps {
            let options = options
                .as_object()
                .ok_or_else(|| format!("app_options.{name} must be an object"))?;
            let Some(value) = options.get("inline_preedit") else {
                continue;
            };
            let enabled = value
                .as_bool()
                .ok_or_else(|| format!("app_options.{name}.inline_preedit must be a boolean"))?;
            if !enabled {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn application_value(&self, executable: &str, option: &str) -> Result<Option<&Value>, String> {
        let apps = self
            .0
            .get("app_options")
            .and_then(Value::as_object)
            .ok_or("app_options must be an object")?;
        let application = apps.get(executable).or_else(|| {
            apps.iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(executable))
                .map(|(_, value)| value)
        });
        match application {
            Some(value) => value
                .as_object()
                .ok_or_else(|| format!("app_options.{executable} must be an object"))
                .map(|options| options.get(option)),
            None => Ok(None),
        }
    }

    /// 读取指定主题的设置。
    ///
    /// 设置值按 `Deserialize` 反序列化。调用前应已通过
    /// [`with_theme_defaults`](Self::with_theme_defaults) 合并主题默认值。
    ///
    /// # Errors
    ///
    /// 配置路径无效，或现有值无法反序列化为 `T` 时返回错误。
    pub fn theme_settings<T: DeserializeOwned>(&self, name: &str) -> Result<T, String> {
        let key = serde_json::to_string(name).map_err(|e| e.to_string())?;
        self.required(&format!(".themeSettings[{key}]"))
    }

    /// 为指定主题叠加默认设置，并返回新的配置快照。
    ///
    /// 默认对象作为基础，现有 `themeSettings.<name>` 作为覆盖值；递归合并规则由
    /// [`merge`] 定义。输入快照保持不变。
    ///
    /// # Errors
    ///
    /// 默认值、配置根或对应主题设置不是 JSON 对象时返回错误。
    pub fn with_theme_defaults(&self, name: &str, mut defaults: Value) -> Result<Self, String> {
        if !defaults.is_object() {
            return Err(format!("theme {name} defaults must be an object"));
        }
        let mut root = (*self.0).clone();
        let root_object = root
            .as_object_mut()
            .ok_or("configuration root must be an object")?;
        let themes = root_object
            .entry("themeSettings")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .ok_or("themeSettings must be an object")?;
        if let Some(overrides) = themes.get(name) {
            if !overrides.is_object() {
                return Err(format!("themeSettings.{name} must be an object"));
            }
            merge(&mut defaults, overrides.clone());
        }
        themes.insert(name.into(), defaults);
        Ok(Self::new(root))
    }
}

/// 将 JSON 覆盖值合并到基础值中。
///
/// 两侧均为对象时递归合并；数组、标量和显式 `null` 均由覆盖值整体替换基础值。
pub fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch,
    }
}

/// 使用受限的 jq 风格路径查询 JSON 值。
///
/// 支持对象属性（`.name`）、带引号的对象键（`["a.b"]`）和数组下标（`[0]`），不支持
/// 过滤器或其他表达式。即使中途遇到缺失属性，也会继续验证路径语法。
///
/// `.` 表示根值；空路径无效。返回引用借用 `root`。
///
/// # Errors
///
/// 路径超过 4096 字节或语法不合法时返回错误。
pub fn query<'a>(root: &'a Value, path: &str) -> Result<Option<&'a Value>, String> {
    if path.len() > 4096 {
        return Err("configuration path exceeds 4096 bytes".into());
    }
    if path == "." {
        return Ok(Some(root));
    }
    let mut rest = path;
    let mut current = Some(root);
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(['.', '[']).unwrap_or(tail.len());
            let key = &tail[..end];
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err("expected a property name after '.'".into());
            }
            current = current.and_then(|v| v.as_object()?.get(key));
            rest = &tail[end..];
        } else if let Some(tail) = rest.strip_prefix('[') {
            if tail.starts_with('"') {
                let mut stream = serde_json::Deserializer::from_str(tail).into_iter::<String>();
                let key = stream
                    .next()
                    .ok_or("missing quoted property")?
                    .map_err(|e| e.to_string())?;
                rest = tail[stream.byte_offset()..]
                    .strip_prefix(']')
                    .ok_or("expected ']' after property")?;
                current = current.and_then(|v| v.as_object()?.get(&key));
            } else {
                let end = tail.find(']').ok_or("expected ']' after index")?;
                let text = &tail[..end];
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("invalid array index".into());
                }
                let index: usize = text.parse().map_err(|_| "array index too large")?;
                current = current.and_then(|v| v.as_array()?.get(index));
                rest = &tail[end + 1..];
            }
        } else {
            return Err("expected '.' or '[' in configuration path".into());
        }
    }
    if path.is_empty() {
        return Err("empty configuration path; use '.' for root".into());
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    #[test]
    fn application_preedit_overrides_global_and_enables_early_negotiation() {
        for global in [true, false] {
            let config = super::ConfigSnapshot::new(serde_json::json!({
                "inline_preedit": global,
                "app_options": {
                    "cmd.exe": {"inline_preedit": false},
                    "editor.exe": {"inline_preedit": true},
                    "other.exe": {"ascii_mode": true}
                }
            }));
            assert!(!config.app_bool("CMD.EXE", "inline_preedit").unwrap());
            assert!(config.app_bool("editor.exe", "inline_preedit").unwrap());
            assert_eq!(
                config.app_bool("other.exe", "inline_preedit").unwrap(),
                global
            );
            assert_eq!(
                config.app_bool("unknown.exe", "inline_preedit").unwrap(),
                global
            );
            assert!(config.needs_external_preedit().unwrap());
        }
        let defaults = super::ConfigSnapshot::new(serde_json::json!({
            "inline_preedit": true,
            "app_options": {}
        }));
        assert!(defaults.app_bool("cmd.exe", "inline_preedit").unwrap());
        assert!(!defaults.needs_external_preedit().unwrap());
    }
    #[test]
    fn application_defaults_distinguish_false_from_missing() {
        let config = super::ConfigSnapshot::new(serde_json::json!({
            "app_options": {"cmd.exe": {"ascii_mode": true}, "editor.exe": {"ascii_mode": false}}
        }));
        assert!(config.app_bool("CMD.EXE", "ascii_mode").unwrap());
        assert!(!config.app_bool("editor.exe", "ascii_mode").unwrap());
        assert!(config.app_bool("unknown.exe", "ascii_mode").is_err());
        let config = super::ConfigSnapshot::new(serde_json::json!({
            "ascii_mode": true, "inline_preedit": false,
            "app_options": {"editor.exe": {"ascii_mode": false}}
        }));
        assert!(config.app_bool("unknown.exe", "ascii_mode").unwrap());
        assert!(!config.app_bool("EDITOR.EXE", "ascii_mode").unwrap());
        assert!(!config.required::<bool>(".inline_preedit").unwrap());
        for global in [false, true] {
            let config = super::ConfigSnapshot::new(serde_json::json!({
                "ascii_mode": global, "inline_preedit": global,
                "app_options": {"empty.exe": {}, "explicit.exe": {
                    "ascii_mode": false, "inline_preedit": false
                }}
            }));
            assert_eq!(config.app_bool("EMPTY.EXE", "ascii_mode").unwrap(), global);
            assert_eq!(
                config.app_bool("EMPTY.EXE", "inline_preedit").unwrap(),
                global
            );
            assert!(!config.app_bool("explicit.exe", "ascii_mode").unwrap());
            assert!(!config.app_bool("explicit.exe", "inline_preedit").unwrap());
        }
    }
    use super::*;
    #[test]
    fn theme_defaults_are_overlaid_without_changing_the_source() {
        let source = ConfigSnapshot::new(serde_json::json!({
            "inline_preedit": true,
            "themeSettings": {"abc": {"antialiasing": false, "nested": {"b": 3}}}
        }));
        let merged = source
            .with_theme_defaults(
                "abc",
                serde_json::json!({
                    "antialiasing": true, "nested": {"a": 1, "b": 2}
                }),
            )
            .unwrap();
        assert_eq!(
            merged
                .get::<bool>(".themeSettings.abc.antialiasing")
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            merged.get::<i32>(".themeSettings.abc.nested.a").unwrap(),
            Some(1)
        );
        assert_eq!(
            merged.get::<i32>(".themeSettings.abc.nested.b").unwrap(),
            Some(3)
        );
        assert!(
            source
                .query(".themeSettings.abc.nested.a")
                .unwrap()
                .is_none()
        );
        assert_eq!(merged.get::<bool>(".inline_preedit").unwrap(), Some(true));
        let fallback = source
            .with_theme_defaults("ten", serde_json::json!({"size": 12}))
            .unwrap();
        assert_eq!(
            fallback.get::<i32>(".themeSettings.ten.size").unwrap(),
            Some(12)
        );
    }
    #[test]
    fn paths_preserve_objects_null_and_missing() {
        let root = serde_json::json!({"a.b": [null, {"color":"red"}]});
        assert_eq!(
            query(&root, "[\"a.b\"][1].color").unwrap(),
            Some(&serde_json::json!("red"))
        );
        assert_eq!(query(&root, "[\"a.b\"][0]").unwrap(), Some(&Value::Null));
        assert!(query(&root, ".missing").unwrap().is_none());
        assert!(query(&root, ".missing | anything").is_err());
        assert_eq!(query(&root, ".").unwrap(), Some(&root));
    }
}
