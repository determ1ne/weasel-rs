//! Read-only host-owned data. Guests query values, never parse JSON or own host pointers.
//! CandidateView uses typed accessors; scope 1 is themeSettings.wasm.modules.<id>.config.
//! Scope 2 contains supported global presentation settings (currently preedit_type).
use crate::abi::{ConfigScope, DataKind, ViewField, ViewStringField};
use crate::protocol::{IMPORT_MODULE, MEMORY};
use crate::runtime::{HostState, read_wasm_string};
use serde_json::Value;
use wasmtime::{Caller, Linker};

fn path(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> wasmtime::Result<String> {
    caller.data_mut().charge(0)?;
    if !(0..=1024).contains(&len) {
        return Err(wasmtime::format_err!("data path exceeds 1024 UTF-8 bytes"));
    }
    read_wasm_string(caller, ptr, len)
        .ok_or_else(|| wasmtime::format_err!("invalid data path memory or UTF-8"))
}

fn value<'a>(state: &'a HostState, scope: i32, path: &str) -> Option<&'a Value> {
    match ConfigScope::try_from(scope).ok()? {
        ConfigScope::Module => &state.options,
        ConfigScope::Global => &state.settings,
    }
    .pointer(path)
}

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
