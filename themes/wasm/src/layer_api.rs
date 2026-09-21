//! 可事务回滚的装饰层 ABI；不暴露 HWND、COM 对象或鼠标命中变换。
use crate::{
    abi::{Easing, LayerProperty as Property, LayerStop},
    layers::{LayerMotion, LayerOperation, LayerProperty, LayerState, MAX_LAYERS},
    runtime::HostState,
};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Linker};

fn editing(state: &mut HostState) -> wasmtime::Result<()> {
    state.charge(0)?;
    if !state.frame_open {
        return Err(wasmtime::format_err!("layers require theme_event"));
    }
    state.layers_edited = true;
    Ok(())
}
fn property(value: i32) -> wasmtime::Result<LayerProperty> {
    Ok(
        match Property::try_from(value)
            .map_err(|_| wasmtime::format_err!("invalid layer property"))?
        {
            Property::Opacity => LayerProperty::Opacity,
            Property::OffsetX => LayerProperty::OffsetX,
            Property::OffsetY => LayerProperty::OffsetY,
            Property::ScaleX => LayerProperty::ScaleX,
            Property::ScaleY => LayerProperty::ScaleY,
        },
    )
}
fn checked_value(property: LayerProperty, value: f32) -> wasmtime::Result<()> {
    let valid = value.is_finite()
        && match property {
            LayerProperty::Opacity => (0.0..=1.0).contains(&value),
            LayerProperty::OffsetX | LayerProperty::OffsetY => value.abs() <= 16384.0,
            LayerProperty::ScaleX | LayerProperty::ScaleY => (0.0..=16.0).contains(&value),
        };
    if valid {
        Ok(())
    } else {
        Err(wasmtime::format_err!("invalid layer value"))
    }
}
fn motion(
    state: &mut HostState,
    id: i32,
    p: i32,
    to: f32,
    duration: f64,
    easing: i32,
    operation: LayerOperation,
) -> wasmtime::Result<()> {
    editing(state)?;
    let property = property(p)?;
    let easing = Easing::try_from(easing).map_err(|_| wasmtime::format_err!("invalid easing"))?;
    if !(0.0..=3_600_000.0).contains(&duration) {
        return Err(wasmtime::format_err!("invalid animation duration"));
    }
    checked_value(property, to)?;
    state.motion_revision = state
        .motion_revision
        .checked_add(1)
        .ok_or_else(|| wasmtime::format_err!("motion revision exhausted"))?;
    let layer = state
        .layers
        .layers
        .iter_mut()
        .find(|l| l.id as i32 == id)
        .ok_or_else(|| wasmtime::format_err!("unknown layer"))?;
    let now = Instant::now();
    let old = layer.motions.iter().position(|m| m.property == property);
    let default = match property {
        LayerProperty::OffsetX | LayerProperty::OffsetY => 0.0,
        _ => 1.0,
    };
    let prior = old.map(|i| layer.motions[i]);
    let same_event = prior.is_some_and(|m| m.revision > state.event_revision);
    let snap_from =
        same_event && prior.is_some_and(|m| m.operation != LayerOperation::Animate || m.snap_from);
    let from = prior.map_or(default, |m| {
        if same_event && m.operation == LayerOperation::Animate {
            m.from
        } else {
            m.value_at(now)
        }
    });
    let target = match operation {
        LayerOperation::StopCurrent => from,
        LayerOperation::StopEnd => old.map_or(from, |i| layer.motions[i].to),
        _ => to,
    };
    let value = LayerMotion {
        snap_from,
        property,
        revision: state.motion_revision,
        operation,
        from,
        to: target,
        start: now,
        deadline: now + Duration::from_secs_f64(duration / 1000.0),
        easing,
    };
    if let Some(index) = old {
        layer.motions[index] = value;
    } else {
        layer.motions.push(value);
    }
    Ok(())
}
pub fn register(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    let module = crate::protocol::IMPORT_MODULE;
    linker.func_wrap(
        module,
        "layer_z_index",
        |mut c: Caller<'_, HostState>, id: i32, z_index: i32| -> wasmtime::Result<()> {
            let s = c.data_mut();
            editing(s)?;
            let layer = s
                .layers
                .layers
                .iter_mut()
                .find(|l| l.id as i32 == id)
                .ok_or_else(|| wasmtime::format_err!("unknown layer"))?;
            layer.z_index = z_index;
            Ok(())
        },
    )?;
    linker.func_wrap(
        module,
        "layer_clip",
        |mut c: Caller<'_, HostState>,
         id: i32,
         enabled: i32,
         x: f32,
         y: f32,
         w: f32,
         h: f32|
         -> wasmtime::Result<()> {
            let s = c.data_mut();
            editing(s)?;
            if !matches!(enabled, 0 | 1)
                || (enabled != 0
                    && (![x, y, w, h]
                        .iter()
                        .all(|v| v.is_finite() && v.abs() <= 16384.0)
                        || w <= 0.0
                        || h <= 0.0))
            {
                return Err(wasmtime::format_err!("invalid layer clip"));
            }
            let layer = s
                .layers
                .layers
                .iter_mut()
                .find(|l| l.id as i32 == id)
                .ok_or_else(|| wasmtime::format_err!("unknown layer"))?;
            layer.clip = (enabled != 0).then_some(crate::protocol::Rect { x, y, w, h });
            Ok(())
        },
    )?;
    linker.func_wrap(
        module,
        "layer_interactive",
        |mut c: Caller<'_, HostState>, id: i32, enabled: i32| -> wasmtime::Result<()> {
            let s = c.data_mut();
            editing(s)?;
            if !matches!(enabled, 0 | 1) {
                return Err(wasmtime::format_err!("invalid interaction flag"));
            }
            let layer = s
                .layers
                .layers
                .iter_mut()
                .find(|l| l.id as i32 == id)
                .ok_or_else(|| wasmtime::format_err!("unknown layer"))?;
            layer.interactive = enabled != 0;
            if enabled == 0 && s.pressed_target.is_some_and(|t| t.0 as i32 == id) {
                s.pressed_target = None;
            }
            Ok(())
        },
    )?;
    linker.func_wrap(
        module,
        "layer_content",
        |mut c: Caller<'_, HostState>, id: i32, w: f32, h: f32| -> wasmtime::Result<()> {
            let s = c.data_mut();
            editing(s)?;
            if s.draw_depth != 0 {
                return Err(wasmtime::format_err!(
                    "balance draw stack before switching layers"
                ));
            }
            if id == 0 {
                s.selected_layer = None;
                return Ok(());
            }
            if id < 0
                || !w.is_finite()
                || !h.is_finite()
                || w <= 0.0
                || h <= 0.0
                || w > 8192.0
                || h > 8192.0
            {
                return Err(wasmtime::format_err!("invalid layer geometry"));
            }
            if let Some(l) = s.layers.layers.iter_mut().find(|l| l.id == id as u32) {
                l.commands.clear();
                l.regions.clear();
                l.size = (w, h);
            } else {
                if s.layers.layers.len() >= MAX_LAYERS {
                    return Err(wasmtime::format_err!("layer quota exceeded"));
                }
                s.motion_revision = s
                    .motion_revision
                    .checked_add(1)
                    .ok_or_else(|| wasmtime::format_err!("layer generation exhausted"))?;
                s.layers.layers.push(LayerState {
                    z_index: 0,
                    generation: s.motion_revision,
                    id: id as u32,
                    size: (w, h),
                    commands: Vec::new(),
                    clip: None,
                    interactive: false,
                    regions: Vec::new(),
                    motions: Vec::new(),
                });
            }
            s.selected_layer = Some(id as u32);
            Ok(())
        },
    )?;
    linker.func_wrap(
        module,
        "layer_remove",
        |mut c: Caller<'_, HostState>, id: i32| -> wasmtime::Result<()> {
            let s = c.data_mut();
            editing(s)?;
            if id <= 0 || s.selected_layer == Some(id as u32) {
                return Err(wasmtime::format_err!("invalid layer removal"));
            }
            s.layers.layers.retain(|l| l.id != id as u32);
            if s.pressed_target.is_some_and(|t| t.0 as i32 == id) {
                s.pressed_target = None;
            }
            Ok(())
        },
    )?;
    linker.func_wrap(
        module,
        "layer_set",
        |mut c: Caller<'_, HostState>, id: i32, p: i32, value: f32| {
            motion(
                c.data_mut(),
                id,
                p,
                value,
                0.0,
                Easing::Linear as i32,
                LayerOperation::Set,
            )
        },
    )?;
    linker.func_wrap(
        module,
        "layer_animate",
        |mut c: Caller<'_, HostState>, id: i32, p: i32, to: f32, duration: f64, easing: i32| {
            motion(
                c.data_mut(),
                id,
                p,
                to,
                duration,
                easing,
                LayerOperation::Animate,
            )
        },
    )?;
    linker.func_wrap(
        module,
        "layer_stop",
        |mut c: Caller<'_, HostState>, id: i32, p: i32, behavior: i32| -> wasmtime::Result<()> {
            let operation = match LayerStop::try_from(behavior)
                .map_err(|_| wasmtime::format_err!("invalid stop behavior"))?
            {
                LayerStop::Current => LayerOperation::StopCurrent,
                LayerStop::End => LayerOperation::StopEnd,
            };
            // Dummy value respects the property domain; stop derives its target from the previous motion.
            motion(
                c.data_mut(),
                id,
                p,
                0.0,
                0.0,
                Easing::Linear as i32,
                operation,
            )
        },
    )?;
    Ok(())
}
