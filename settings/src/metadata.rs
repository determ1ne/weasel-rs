//! 有资源上限的离线元数据解析与验证；注解仅作为数据，不执行代码。
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 8192;
const MAX_FIELDS: usize = 512;
const MAX_STRING: usize = 16384;
const MAX_WORK: usize = 131072;

#[derive(Debug, Clone)]
pub struct Metadata {
    pub defaults: Value,
    pub richschema: Value,
}

impl Metadata {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("metadata exceeds 1 MiB".into());
        }
        // 在 serde 分配树之前检查嵌套深度，格式错误的 JSON 也不能绕过限制。
        let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
        for &b in bytes {
            if quoted {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    quoted = false;
                }
            } else {
                match b {
                    b'"' => quoted = true,
                    b'{' | b'[' => {
                        depth += 1;
                        if depth > MAX_DEPTH {
                            return Err("metadata depth exceeds 32".into());
                        }
                    }
                    b'}' | b']' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        let mut package: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("metadata JSON: {e}"))?;
        bounded(&package)?;
        let obj = package
            .as_object_mut()
            .ok_or("metadata must be an object")?;
        if obj.get("formatVersion").and_then(Value::as_u64) != Some(1) {
            return Err("metadata formatVersion must be 1".into());
        }
        if obj
            .keys()
            .any(|k| !matches!(k.as_str(), "formatVersion" | "defaults" | "richschema"))
        {
            return Err("unsupported metadata member".into());
        }
        let defaults = obj.remove("defaults").ok_or("missing defaults")?;
        let richschema = obj.remove("richschema").ok_or("missing richschema")?;
        Self::from_parts(defaults, richschema)
    }

    pub fn from_parts(defaults: Value, richschema: Value) -> Result<Self, String> {
        let metadata = Self {
            defaults,
            richschema,
        };
        let engine = metadata.engine()?;
        // 空对象或仅含编辑器提示的对象表示没有完整默认配置。
        if metadata
            .defaults
            .as_object()
            .unwrap()
            .keys()
            .any(|k| k != "$schema")
        {
            engine
                .validate(&metadata.defaults)
                .map_err(|e| format!("defaults: {e}"))?;
        }
        Ok(metadata)
    }

    pub fn validate(&self, value: &Value) -> Result<(), String> {
        bounded(value)?;
        // 公共字段可能被调用者修改，每次验证都重新检查元数据。
        self.engine()?.validate(value)
    }

    fn engine(&self) -> Result<Engine<'_>, String> {
        let mut nodes = 2; // package object and formatVersion
        let mut bytes = 64; // conservative package wrapper allowance
        bounds(&self.defaults, 1, &mut nodes, &mut bytes)?;
        bounds(&self.richschema, 1, &mut nodes, &mut bytes)?;
        if !self.defaults.is_object() {
            return Err("defaults must be an object".into());
        }
        let rich = self
            .richschema
            .as_object()
            .ok_or("richschema must be an object")?;
        if rich.get("formatVersion").and_then(Value::as_u64) != Some(1) {
            return Err("richschema formatVersion must be 1".into());
        }
        if rich
            .keys()
            .any(|k| !matches!(k.as_str(), "formatVersion" | "schema" | "ui"))
        {
            return Err("unsupported richschema member".into());
        }
        let ui = rich
            .get("ui")
            .and_then(Value::as_object)
            .ok_or("ui must be an object")?;
        if ui.keys().any(|k| k != "fields") {
            return Err("unsupported ui member".into());
        }
        let fields = ui
            .get("fields")
            .and_then(Value::as_object)
            .ok_or("ui.fields must be an object")?;
        for (pointer, field) in fields {
            if !valid_pointer(pointer) {
                return Err(format!("invalid UI field pointer: {pointer}"));
            }
            let annotations = field.as_object().ok_or("UI field must be an object")?;
            for (key, value) in annotations {
                let safe = match key.as_str() {
                    "order" => value.is_number(),
                    "title" | "label" | "description" | "group" | "placeholder" | "widget" => {
                        value.is_string()
                    }
                    "hidden" | "readOnly" => value.is_boolean(),
                    _ => false,
                };
                if !safe {
                    return Err(format!("unsupported or invalid UI annotation: {key}"));
                }
            }
        }
        let root = rich.get("schema").ok_or("missing richschema.schema")?;
        let mut engine = Engine {
            root,
            patterns: HashMap::new(),
            property_maps: HashSet::new(),
            property_count: 0,
        };
        let mut work = MAX_WORK;
        engine.inspect(root, 0, &mut Vec::new(), &mut work)?;
        Ok(engine)
    }
}

fn bounded(value: &Value) -> Result<(), String> {
    bounds(value, 0, &mut 0, &mut 0)
}

fn bounds(value: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    *bytes += 8;
    if depth > MAX_DEPTH || *nodes > MAX_NODES {
        return Err("JSON depth/node limit exceeded".into());
    }
    match value {
        Value::String(s) => string_bound(s, bytes)?,
        Value::Array(values) => {
            for v in values {
                bounds(v, depth + 1, nodes, bytes)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAX_FIELDS {
                return Err("object properties/fields exceed 512".into());
            }
            for (k, v) in values {
                string_bound(k, bytes)?;
                bounds(v, depth + 1, nodes, bytes)?;
            }
        }
        _ => *bytes += 24,
    }
    if *bytes > MAX_BYTES {
        return Err("JSON size budget exceeds 1 MiB".into());
    }
    Ok(())
}

fn string_bound(s: &str, bytes: &mut usize) -> Result<(), String> {
    if s.len() > MAX_STRING {
        return Err("string exceeds 16384 bytes".into());
    }
    // 保守估计序列化大小，包括 JSON 转义开销。
    *bytes += 2 + s
        .bytes()
        .map(|b| match b {
            0..=31 => 6,
            b'"' | b'\\' => 2,
            _ => 1,
        })
        .sum::<usize>();
    Ok(())
}

fn spend(work: &mut usize, amount: usize) -> Result<(), String> {
    *work = work
        .checked_sub(amount)
        .ok_or("schema work limit exceeded")?;
    Ok(())
}

fn valid_pointer(s: &str) -> bool {
    s.starts_with('/')
        && s.split('/').all(|part| {
            let mut chars = part.chars();
            while let Some(c) = chars.next() {
                if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                    return false;
                }
            }
            true
        })
}

struct Engine<'a> {
    root: &'a Value,
    patterns: HashMap<String, Regex>,
    property_maps: HashSet<*const Value>,
    property_count: usize,
}

impl<'a> Engine<'a> {
    fn resolve(&self, reference: &Value) -> Result<&'a Value, String> {
        let r = reference.as_str().ok_or("$ref must be a string")?;
        let pointer = r
            .strip_prefix('#')
            .filter(|p| valid_pointer(p))
            .ok_or("only local #/ JSON Pointer references are supported")?;
        self.root
            .pointer(pointer)
            .ok_or_else(|| format!("unresolved $ref: {r}"))
    }

    fn inspect(
        &mut self,
        schema: &Value,
        depth: usize,
        stack: &mut Vec<*const Value>,
        work: &mut usize,
    ) -> Result<(), String> {
        spend(work, 1)?;
        if depth > MAX_DEPTH {
            return Err("schema/reference depth exceeds 32".into());
        }
        let ptr = schema as *const Value;
        if stack.contains(&ptr) {
            return Err("cyclic schema reference".into());
        }
        if schema.is_boolean() {
            return Ok(());
        }
        let obj = schema
            .as_object()
            .ok_or("schema must be an object or boolean")?;
        stack.push(ptr);
        for (key, v) in obj {
            spend(work, 1)?;
            let valid = match key.as_str() {
                "$ref" => {
                    let target = self.resolve(v)?;
                    self.inspect(target, depth + 1, stack, work)?;
                    true
                }
                "$defs" | "definitions" | "properties" | "patternProperties" => {
                    let entries = v
                        .as_object()
                        .ok_or_else(|| format!("{key} must be an object"))?;
                    if matches!(key.as_str(), "properties" | "patternProperties")
                        && self.property_maps.insert(v as *const Value)
                    {
                        self.property_count += entries.len();
                        if self.property_count > MAX_FIELDS {
                            return Err("total schema properties exceed 512".into());
                        }
                    }
                    for (name, child) in entries {
                        if key == "patternProperties" {
                            self.compile(name, work)?;
                        }
                        self.inspect(child, depth + 1, stack, work)?;
                    }
                    true
                }
                "additionalProperties" | "propertyNames" => {
                    self.inspect(v, depth + 1, stack, work)?;
                    true
                }
                "oneOf" | "anyOf" | "allOf" => {
                    let entries = v
                        .as_array()
                        .filter(|a| !a.is_empty())
                        .ok_or_else(|| format!("{key} must be a nonempty array"))?;
                    for child in entries {
                        self.inspect(child, depth + 1, stack, work)?;
                    }
                    true
                }
                "type" => {
                    let legal = |v: &Value| {
                        matches!(
                            v.as_str(),
                            Some(
                                "null"
                                    | "boolean"
                                    | "object"
                                    | "array"
                                    | "number"
                                    | "integer"
                                    | "string"
                            )
                        )
                    };
                    legal(v)
                        || v.as_array().is_some_and(|a| {
                            !a.is_empty()
                                && a.iter().all(legal)
                                && a.iter().collect::<HashSet<_>>().len() == a.len()
                        })
                }
                "required" => v.as_array().is_some_and(|a| {
                    a.iter().all(Value::is_string)
                        && a.iter().collect::<HashSet<_>>().len() == a.len()
                }),
                "enum" => v.as_array().is_some_and(|a| !a.is_empty()),
                "const" | "default" => true,
                "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => v.is_number(),
                "minLength" | "maxLength" => v.as_u64().is_some(),
                "pattern" => {
                    self.compile(v.as_str().ok_or("pattern must be a string")?, work)?;
                    true
                }
                "$schema" | "$id" | "$comment" | "title" | "description" => v.is_string(),
                "readOnly" | "writeOnly" | "deprecated" => v.is_boolean(),
                "examples" => v.is_array(),
                _ => return Err(format!("unsupported schema keyword: {key}")),
            };
            if !valid {
                return Err(format!("invalid schema keyword: {key}"));
            }
        }
        stack.pop();
        Ok(())
    }

    fn compile(&mut self, pattern: &str, work: &mut usize) -> Result<(), String> {
        if !self.patterns.contains_key(pattern) {
            spend(work, 2048 + pattern.len())?;
            let regex = RegexBuilder::new(pattern)
                .size_limit(65536)
                .dfa_size_limit(65536)
                .nest_limit(32)
                .build()
                .map_err(|e| format!("invalid/oversized regex: {e}"))?;
            self.patterns.insert(pattern.to_owned(), regex);
        }
        Ok(())
    }

    fn matches(&self, pattern: &str, text: &str, work: &mut usize) -> Result<bool, String> {
        spend(work, (pattern.len() + 1).saturating_mul(text.len() + 1))?;
        Ok(self.patterns[pattern].is_match(text))
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        let mut work = MAX_WORK;
        match self.check(self.root, value, "$", 0, &mut work)? {
            None => Ok(()),
            Some(message) => Err(message),
        }
    }

    // 外层 Err 表示资源超限，不能被组合分支吞掉；内层 Some 表示普通不匹配。
    fn check(
        &self,
        schema: &Value,
        value: &Value,
        path: &str,
        depth: usize,
        work: &mut usize,
    ) -> Result<Option<String>, String> {
        spend(work, 1)?;
        if depth > MAX_DEPTH {
            return Err("validation depth exceeds 32".into());
        }
        let fail = |message: &str| Ok(Some(format!("{path}: {message}")));
        if let Some(allowed) = schema.as_bool() {
            return if allowed {
                Ok(None)
            } else {
                fail("value is forbidden")
            };
        }
        let obj = schema.as_object().ok_or("invalid schema")?;
        if let Some(r) = obj.get("$ref") {
            if let Some(e) = self.check(self.resolve(r)?, value, path, depth + 1, work)? {
                return Ok(Some(e));
            }
        }
        if let Some(t) = obj.get("type") {
            let matches = |t: &Value| match t.as_str().unwrap_or("") {
                "null" => value.is_null(),
                "boolean" => value.is_boolean(),
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
                _ => false,
            };
            if !matches(t) && !t.as_array().is_some_and(|a| a.iter().any(matches)) {
                return fail(&format!("expected type {t}"));
            }
        }
        if let Some(expected) = obj.get("const") {
            if !equal(value, expected, work)? {
                return fail("does not match const");
            }
        }
        if let Some(options) = obj.get("enum").and_then(Value::as_array) {
            let mut found = false;
            for option in options {
                if equal(value, option, work)? {
                    found = true;
                    break;
                }
            }
            if !found {
                return fail("not an allowed enum value");
            }
        }
        if let Some(n) = value.as_number() {
            for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
                if let Some(limit) = obj.get(key).and_then(Value::as_number) {
                    let cmp = number_cmp(n, limit);
                    let valid = match key {
                        "minimum" => !cmp.is_lt(),
                        "maximum" => !cmp.is_gt(),
                        "exclusiveMinimum" => cmp.is_gt(),
                        _ => cmp.is_lt(),
                    };
                    if !valid {
                        return fail(&format!("violates {key} {limit}"));
                    }
                }
            }
        }
        if let Some(s) = value.as_str() {
            spend(work, s.len() + 1)?;
            let len = s.chars().count() as u64;
            for key in ["minLength", "maxLength"] {
                if let Some(limit) = obj.get(key).and_then(Value::as_u64) {
                    if (key == "minLength" && len < limit) || (key == "maxLength" && len > limit) {
                        return fail(&format!("violates {key} {limit}"));
                    }
                }
            }
            if let Some(pattern) = obj.get("pattern").and_then(Value::as_str) {
                if !self.matches(pattern, s, work)? {
                    return fail(&format!("does not match pattern {pattern}"));
                }
            }
        }
        if let Some(values) = value.as_object() {
            if let Some(required) = obj.get("required").and_then(Value::as_array) {
                for key in required {
                    spend(work, 1)?;
                    if !values.contains_key(key.as_str().unwrap()) {
                        return fail(&format!("missing required property {key}"));
                    }
                }
            }
            for (key, child) in values {
                spend(work, key.len() + 1)?;
                let child_path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                if let Some(names) = obj.get("propertyNames") {
                    if let Some(e) = self.check(
                        names,
                        &Value::String(key.clone()),
                        &child_path,
                        depth + 1,
                        work,
                    )? {
                        return Ok(Some(e));
                    }
                }
                let mut covered = false;
                if let Some(property) = obj.get("properties").and_then(|p| p.get(key)) {
                    covered = true;
                    if let Some(e) = self.check(property, child, &child_path, depth + 1, work)? {
                        return Ok(Some(e));
                    }
                }
                if let Some(patterns) = obj.get("patternProperties").and_then(Value::as_object) {
                    for (pattern, property) in patterns {
                        if self.matches(pattern, key, work)? {
                            covered = true;
                            if let Some(e) =
                                self.check(property, child, &child_path, depth + 1, work)?
                            {
                                return Ok(Some(e));
                            }
                        }
                    }
                }
                // 旧版原生主题 schema 未声明根级编辑器提示，也允许保留它。
                if !covered && !(path == "$" && key == "$schema" && child.is_string()) {
                    if let Some(additional) = obj.get("additionalProperties") {
                        if let Some(e) =
                            self.check(additional, child, &child_path, depth + 1, work)?
                        {
                            return Ok(Some(e));
                        }
                    }
                }
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = obj.get(key).and_then(Value::as_array) {
                let mut count = 0;
                let mut first_error = None;
                for branch in branches {
                    match self.check(branch, value, path, depth + 1, work)? {
                        None => count += 1,
                        Some(e) => {
                            if first_error.is_none() {
                                first_error = Some(e);
                            }
                        }
                    }
                }
                let valid = match key {
                    "allOf" => count == branches.len(),
                    "anyOf" => count > 0,
                    _ => count == 1,
                };
                if !valid {
                    return fail(&format!(
                        "{key}: {count}/{} branches matched; {}",
                        branches.len(),
                        first_error.unwrap_or_else(|| "expected exactly one match".into())
                    ));
                }
            }
        }
        Ok(None)
    }
}

fn number_cmp(a: &serde_json::Number, b: &serde_json::Number) -> std::cmp::Ordering {
    let integer = |n: &serde_json::Number| {
        n.as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
    };
    // 避免大整数转 f64 丢失精度；JSON 数值相等也包括 0 与 -0.0。
    let mixed = |i: i128, f: f64| {
        i.cmp(&(f as i128))
            .then_with(|| 0.0f64.partial_cmp(&f.fract()).unwrap())
    };
    match (integer(a), integer(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(a), None) => mixed(a, b.as_f64().unwrap()),
        (None, Some(b)) => mixed(b, a.as_f64().unwrap()).reverse(),
        _ => a
            .as_f64()
            .unwrap()
            .partial_cmp(&b.as_f64().unwrap())
            .unwrap(),
    }
}

fn equal(a: &Value, b: &Value, work: &mut usize) -> Result<bool, String> {
    spend(work, 1)?;
    Ok(match (a, b) {
        (Value::Number(a), Value::Number(b)) => number_cmp(a, b).is_eq(),
        (Value::String(a), Value::String(b)) => {
            spend(work, a.len().min(b.len()))?;
            a == b
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !equal(a, b, work)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Object(a), Value::Object(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (k, a) in a {
                spend(work, k.len() + 1)?;
                let Some(b) = b.get(k) else {
                    return Ok(false);
                };
                if !equal(a, b, work)? {
                    return Ok(false);
                }
            }
            true
        }
        _ => a == b,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(schema: Value) -> Result<Metadata, String> {
        Metadata::from_parts(
            json!({}),
            json!({"formatVersion":1,"schema":schema,"ui":{"fields":{}}}),
        )
    }

    #[test]
    fn rejects_unsafe_schemas() {
        for schema in [
            json!({"$ref":"https://example.com/schema"}),
            json!({"$defs":{"loop":{"$ref":"#/$defs/loop"}}}),
            json!({"$dynamicRef":"#x"}),
            json!({"script":"run"}),
            json!({"type":"string","format":"email"}),
            json!({"pattern":"(?=bad)"}),
            json!({"minimum":"zero"}),
        ] {
            assert!(metadata(schema).is_err());
        }
    }

    #[test]
    fn validates_nested_refs_and_values() {
        let m = metadata(json!({"type":"object","required":["color"],
            "$defs":{"color":{"type":"string","pattern":"^#[0-9a-fA-F]{6}$"}},
            "properties":{"color":{"$ref":"#/$defs/color"},"size":{"type":"integer","minimum":1,"maximum":20}},
            "propertyNames":{"pattern":"^[a-z$]+$"},"additionalProperties":false})).unwrap();
        assert!(m.validate(&json!({"color":"#abcdef","size":12})).is_ok());
        for value in [
            json!({}),
            json!({"color":"red"}),
            json!({"color":"#abcdef","size":21}),
            json!({"color":"#abcdef","extra":true}),
        ] {
            assert!(m.validate(&value).is_err());
        }
    }

    #[test]
    fn resource_limits_and_numeric_equality() {
        assert!(Metadata::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
        let m = metadata(json!({})).unwrap();
        assert!(m.validate(&json!("x".repeat(MAX_STRING + 1))).is_err());
        assert!(m.validate(&json!(vec![0; MAX_NODES])).is_err());
        let mut deep = json!(null);
        for _ in 0..=MAX_DEPTH {
            deep = json!([deep]);
        }
        assert!(m.validate(&deep).is_err());
        // 即便另一分支匹配，正则预算超限仍然必须失败。
        let m = metadata(json!({"anyOf":[true,{"pattern":"a".repeat(100)}]})).unwrap();
        assert!(
            m.validate(&json!("a".repeat(2000)))
                .unwrap_err()
                .contains("work limit")
        );
        assert!(
            metadata(json!({"const":0}))
                .unwrap()
                .validate(&json!(-0.0))
                .is_ok()
        );
        assert!(
            metadata(json!({"maximum":9007199254740992.0}))
                .unwrap()
                .validate(&json!(9007199254740993u64))
                .is_err()
        );
    }

    #[test]
    fn native_metadata_and_defaults() {
        for (name, defaults, richschema) in [
            (
                "main",
                include_str!("../../weasel.json"),
                include_str!("../../weasel.richschema.json"),
            ),
            (
                "abc",
                include_str!("../../themes/abc/src/config.json"),
                include_str!("../../themes/abc/src/config.richschema.json"),
            ),
            (
                "eleven",
                include_str!("../../themes/eleven/src/config.json"),
                include_str!("../../themes/eleven/src/config.richschema.json"),
            ),
            (
                "weaselui",
                include_str!("../../themes/wasm/theme-weaselui/config.json"),
                include_str!("../../themes/wasm/theme-weaselui/config.richschema.json"),
            ),
        ] {
            let m = Metadata::from_parts(
                serde_json::from_str(defaults).unwrap(),
                serde_json::from_str(richschema).unwrap(),
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            m.validate(&m.defaults)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let package =
                json!({"formatVersion":1,"defaults":m.defaults,"richschema":m.richschema});
            Metadata::parse(&serde_json::to_vec(&package).unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let package = json!({"formatVersion":1,"defaults":{"n":0},"richschema":{
            "formatVersion":1,"schema":{"properties":{"n":{"minimum":1}}},"ui":{"fields":{}}}});
        assert!(Metadata::parse(&serde_json::to_vec(&package).unwrap()).is_err());
        assert!(metadata(json!({"required":["n"]})).is_ok());
    }
}
