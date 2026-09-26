//! 向 guest 暴露只读的宿主数据，不传递宿主指针，也不要求 guest 解析 JSON。
//!
//! 候选视图通过按类型区分的查询导入访问。数据作用域 1 对应
//! `themeSettings.wasm.modules.<id>.config`；作用域 2 对应受支持的全局呈现设置，目前包括
//! `preedit_type`。查询路径使用 JSON Pointer 语法，字符串长度和缓冲区容量均以 UTF-8
//! 字节计。
use crate::abi::{ConfigScope, DataKind, ViewField, ViewStringField};
use crate::protocol::{IMPORT_MODULE, MEMORY};
use crate::runtime::{HostState, read_wasm_string};
use serde_json::Value;
use wasmtime::{Caller, Linker};

/// 从 guest 内存读取 JSON Pointer 路径，并执行调用计费和长度校验。
///
/// 长度必须在 0..=1024 字节内；越界或无效内存/UTF-8 会返回 Wasmtime 错误。
fn path(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> wasmtime::Result<String> {
    caller.data_mut().charge(0)?;
    if !(0..=1024).contains(&len) {
        return Err(wasmtime::format_err!("data path exceeds 1024 UTF-8 bytes"));
    }
    read_wasm_string(caller, ptr, len)
        .ok_or_else(|| wasmtime::format_err!("invalid data path memory or UTF-8"))
}

/// 在指定只读配置作用域内借用路径对应的 JSON 值。
///
/// 未知作用域、路径不存在或 JSON Pointer 无匹配项均返回 `None`；返回引用仅在宿主状态
/// 保持借用期间有效，不复制配置数据。
fn value<'a>(state: &'a HostState, scope: i32, path: &str) -> Option<&'a Value> {
    match ConfigScope::try_from(scope).ok()? {
        ConfigScope::Module => &state.options,
        ConfigScope::Global => &state.settings,
    }
    .pointer(path)
}

/// 注册候选视图和只读配置查询导入。
///
/// `view_i64` 与 `view_string` 读取当前宿主快照；没有快照时，数值字段返回 0 或其约定的
/// `-1`，字符串查询返回 `-1`。`data_kind` 区分缺失与 JSON 类型，`data_len` 查询数组、
/// 对象或字符串长度，类型不匹配时返回 `-1`；数值及整数查询在类型不匹配时分别返回
/// NaN 和 0。字符串写入不截断且不追加终止符：容量不足时只返回所需 UTF-8 字节数，
/// 缓冲区足够时才写入 guest 的 `memory`。非法字段、索引、路径和内存通过 Wasmtime 错误
/// 报告；链接器注册失败原样返回。
pub fn register(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    linker.func_wrap(
        IMPORT_MODULE,
        "view_i64",
        |mut c: Caller<'_, HostState>, field: i32, index: i32| -> wasmtime::Result<i64> {
            c.data_mut().charge(0)?;
            let field = ViewField::try_from(field)
                .map_err(|_| wasmtime::format_err!("invalid view field"))?;
            if field != ViewField::ItemEnabled && index != 0 {
                return Err(wasmtime::format_err!("invalid view index"));
            }
            let Some(v) = &c.data().view else {
                return Ok(
                    if matches!(field, ViewField::AsciiMode | ViewField::TotalItemCount) {
                        -1
                    } else {
                        0
                    },
                );
            };
            Ok(match field {
                ViewField::ContentId => v.content_id as i64,
                ViewField::Active => v.active as i64,
                ViewField::Visible => v.visible as i64,
                ViewField::AsciiMode => v.ascii_mode.map_or(-1, i64::from),
                ViewField::ItemCount => v.items.len() as i64,
                ViewField::SelectedIndex => v.selected_index as i64,
                ViewField::PageStart => v.page_start as i64,
                ViewField::TotalItemCount => v.total_item_count.map_or(-1, i64::from),
                ViewField::CanPagePrevious => v.can_page_previous as i64,
                ViewField::CanPageNext => v.can_page_next as i64,
                ViewField::HasPreedit => v.preedit.is_some() as i64,
                ViewField::CursorUtf16 => v.preedit.as_ref().map_or(0, |p| p.cursor as i64),
                ViewField::HasSnapshot => 1,
                ViewField::ItemEnabled => {
                    v.items
                        .get(index as usize)
                        .ok_or_else(|| wasmtime::format_err!("invalid item index"))?
                        .enabled as i64
                }
                ViewField::AnchorValid => v.anchor.as_ref().is_some_and(|a| a.valid) as i64,
                ViewField::AnchorLeft => v.anchor.as_ref().map_or(0, |a| a.left as i64),
                ViewField::AnchorTop => v.anchor.as_ref().map_or(0, |a| a.top as i64),
                ViewField::AnchorRight => v.anchor.as_ref().map_or(0, |a| a.right as i64),
                ViewField::AnchorBottom => v.anchor.as_ref().map_or(0, |a| a.bottom as i64),
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "view_string",
        |mut c: Caller<'_, HostState>,
         field: i32,
         index: i32,
         dst: i32,
         capacity: i32|
         -> wasmtime::Result<i32> {
            c.data_mut().charge(0)?;
            let field = ViewStringField::try_from(field)
                .map_err(|_| wasmtime::format_err!("invalid view string field"))?;
            if capacity < 0 || (field == ViewStringField::Preedit && index != 0) {
                return Err(wasmtime::format_err!("invalid view string query"));
            }
            let Some(v) = &c.data().view else {
                return Ok(-1);
            };
            let text = if field == ViewStringField::Preedit {
                v.preedit.as_ref().map(|p| p.text.as_str())
            } else {
                v.items.get(index as usize).map(|i| {
                    if field == ViewStringField::Primary {
                        i.primary_text.as_str()
                    } else {
                        i.secondary_text.as_str()
                    }
                })
            };
            let Some(text) = text else {
                return Ok(-1);
            };
            let len = text.len();
            if capacity == 0 || (capacity as usize) < len {
                return Ok(len as i32);
            }
            let text = text.to_owned();
            c.data_mut().charge(len)?;
            let memory = c
                .get_export(MEMORY)
                .and_then(|v| v.into_memory())
                .ok_or_else(|| wasmtime::format_err!("missing memory"))?;
            memory.write(&mut c, dst as u32 as usize, text.as_bytes())?;
            Ok(len as i32)
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "data_kind",
        |mut c: Caller<'_, HostState>, scope: i32, p: i32, n: i32| -> wasmtime::Result<i32> {
            let p = path(&mut c, p, n)?;
            Ok(match value(c.data(), scope, &p) {
                None => DataKind::Missing as i32,
                Some(Value::Null) => DataKind::Null as i32,
                Some(Value::Bool(_)) => DataKind::Bool as i32,
                Some(Value::Number(_)) => DataKind::Number as i32,
                Some(Value::String(_)) => DataKind::String as i32,
                Some(Value::Array(_)) => DataKind::Array as i32,
                Some(Value::Object(_)) => DataKind::Object as i32,
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "data_len",
        |mut c: Caller<'_, HostState>, scope: i32, p: i32, n: i32| -> wasmtime::Result<i32> {
            let p = path(&mut c, p, n)?;
            Ok(match value(c.data(), scope, &p) {
                Some(Value::String(v)) => v.len() as i32,
                Some(Value::Array(v)) => v.len() as i32,
                Some(Value::Object(v)) => v.len() as i32,
                _ => -1,
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "data_number",
        |mut c: Caller<'_, HostState>, scope: i32, p: i32, n: i32| -> wasmtime::Result<f64> {
            let p = path(&mut c, p, n)?;
            Ok(value(c.data(), scope, &p)
                .and_then(Value::as_f64)
                .unwrap_or(f64::NAN))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "data_i64",
        |mut c: Caller<'_, HostState>, scope: i32, p: i32, n: i32| -> wasmtime::Result<i64> {
            let p = path(&mut c, p, n)?;
            // Unsigned integers retain all 64 bits; use a guest u64 for content_id.
            Ok(match value(c.data(), scope, &p) {
                Some(Value::Bool(v)) => i64::from(*v),
                Some(Value::Number(v)) => v
                    .as_i64()
                    .or_else(|| v.as_u64().map(|v| v as i64))
                    .unwrap_or(0),
                _ => 0,
            })
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        "data_string",
        |mut c: Caller<'_, HostState>,
         scope: i32,
         p: i32,
         n: i32,
         dst: i32,
         capacity: i32|
         -> wasmtime::Result<i32> {
            let p = path(&mut c, p, n)?;
            let Some(text) = value(c.data(), scope, &p).and_then(Value::as_str) else {
                return Ok(-1);
            };
            let len = text.len();
            // No truncation, no terminator. Insufficient capacity returns the required size.
            if capacity < 0 {
                return Err(wasmtime::format_err!("negative string capacity"));
            }
            if (capacity as usize) < len {
                return Ok(len as i32);
            }
            let text = text.to_owned();
            c.data_mut().charge(len)?;
            let memory = c
                .get_export(MEMORY)
                .and_then(|v| v.into_memory())
                .ok_or_else(|| wasmtime::format_err!("missing memory"))?;
            memory.write(&mut c, dst as u32 as usize, text.as_bytes())?;
            Ok(len as i32)
        },
    )?;
    Ok(())
}
