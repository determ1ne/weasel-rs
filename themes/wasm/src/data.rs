//! Read-only host-owned data. Guests query values, never parse JSON or own host pointers.
//! Scope 0 is the current CandidateView; scope 1 is themeSettings.wasm.modules.<id>.config.
//! Scope 2 contains supported global presentation settings (currently preedit_type).
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
    match scope {
        0 => &state.view,
        1 => &state.options,
        2 => &state.settings,
        _ => return None,
    }
    .pointer(path)
}

pub fn register(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    linker.func_wrap(
        IMPORT_MODULE,
        "data_kind",
        |mut c: Caller<'_, HostState>, scope: i32, p: i32, n: i32| -> wasmtime::Result<i32> {
            let p = path(&mut c, p, n)?;
            Ok(match value(c.data(), scope, &p) {
                None => 0,
                Some(Value::Null) => 1,
                Some(Value::Bool(_)) => 2,
                Some(Value::Number(_)) => 3,
                Some(Value::String(_)) => 4,
                Some(Value::Array(_)) => 5,
                Some(Value::Object(_)) => 6,
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
