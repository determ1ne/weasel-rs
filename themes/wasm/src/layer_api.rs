//! 注册供 WASM 主题使用的保留图层和动画宿主函数。
//!
//! 修改仅能发生在 `theme_event` 打开的帧事务内；宿主将这些改动纳入场景
//! 快照，事务失败时可以回滚。ABI 不暴露 HWND、COM 对象或原生鼠标命中变换。
use crate::{
    abi::{Easing, LayerProperty as Property, LayerStop},
    layers::{LayerMotion, LayerOperation, LayerProperty, LayerState, MAX_LAYERS},
    runtime::HostState,
};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Linker};

/// 验证当前回调处于可编辑的帧事务，并记录本帧改动过图层。
///
/// 事务外调用会失败；这里不自行开启事务，也不改变宿主的计费策略。
fn editing(state: &mut HostState) -> wasmtime::Result<()> {
    state.charge(0)?;
    if !state.frame_open {
        return Err(wasmtime::format_err!("layers require theme_event"));
    }
    state.layers_edited = true;
    Ok(())
}
/// 将 ABI 整数映射为宿主属性；未知枚举值作为 WASM 调用错误返回。
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
/// 按属性的 ABI 数值域校验值，并拒绝 NaN 与无穷值。
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
/// 创建或替换图层某一属性的唯一运动记录。
///
/// 新命令递增修订号；同一事件内的连续动画沿用该事件的逻辑起点，其他
/// 重定向则从宿主单调时钟采样近似当前位置。停止命令的目标由既有运动
/// 推导，`to` 仅用于通过属性域校验。找不到图层、参数越界或修订号耗尽均失败。
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
/// 将装饰层宿主函数注册到主题 ABI 对应的导入模块。
///
/// 回调通过 Wasmtime 的 `Caller` 访问当前实例独占的 [`HostState`]，不在
/// 独立线程中保存状态。每个调用都会校验事务及参数；错误交由运行时作为
/// 导入失败处理，由外层事件事务决定是否回滚。
pub fn register(linker: &mut Linker<HostState>) -> wasmtime::Result<()> {
    let module = crate::protocol::IMPORT_MODULE;
    // 图层按 z-index、再按创建代次排序；此处只更新排序键。
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
    // 裁剪矩形固定在内容坐标系；关闭裁剪时矩形参数不参与验证或存储。
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
    // 禁用交互会同步清除指向该图层的按下目标，避免后续释放事件落到旧目标。
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
    // 选择图层并开始替换其绘制内容；传入 0 仅退出图层上下文。
    // 切换前必须平衡绘制状态栈，新图层受数量配额限制。
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
    // 删除未选中的正 ID 图层，并清理指向它的按下目标；未知 ID 按幂等删除处理。
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
    // 即时设置属性；仍通过 motion 统一维护属性唯一性和修订顺序。
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
    // 用 ABI 指定的缓动曲线将属性过渡到目标值。
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
    // 冻结当前呈现值，或结束到原动画目标；实际语义由 LayerStop 决定。
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
