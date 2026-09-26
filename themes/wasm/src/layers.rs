//! 保留图层的场景模型、动画采样、命中测试与资源配额校验。
//!
//! 场景快照由运行时纳入帧事务；图层 ID 为正数，创建后在其生命周期内稳定，
//! 排序按 z-index 和创建代次进行，所有图层都绘制在主内容之上。绘制命令使用
//! 图层本地 DIP 坐标，偏移相对内容原点；裁剪和命中区域使用内容坐标。
use crate::protocol::DrawCommand;
use std::time::{Duration, Instant};

/// 单个场景最多允许保留的图层数。
pub const MAX_LAYERS: usize = 8;
/// 场景中图层表面的总像素上限；按 32 位 BGRA 计为 64 MiB。
pub const MAX_LAYER_PIXELS: u64 = 16 * 1024 * 1024;
/// 单个场景的保留图层绘制命令总数上限。
pub const MAX_LAYER_COMMANDS: usize = 8192;

/// 可事务快照和验证的完整保留图层集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerScene {
    /// 按创建顺序保存图层；绘制顺序由 [`LayerScene::ordered`] 单独计算。
    pub layers: Vec<LayerState>,
}

/// 一个独立绘制、变换、裁剪和交互的保留图层。
#[derive(Debug, Clone, PartialEq)]
pub struct LayerState {
    /// 每次创建的新身份，防止复用ID继承旧表面和动画。
    pub generation: u64,
    /// 主题提供的正数标识；同一场景内必须唯一。
    pub id: u32,
    /// 越大越靠前；相同值按创建顺序。仅在保留图层之间排序，均在主画面之上。
    pub z_index: i32,
    /// 以 DIP 表示的图层本地尺寸；对应表面像素数受场景总配额限制。
    pub size: (f32, f32),
    /// 在图层本地坐标系中按序回放的绘制命令。
    pub commands: Vec<DrawCommand>,
    /// 固定在内容坐标系，不随本层位移/缩放移动。
    pub clip: Option<crate::protocol::Rect>,
    /// 是否参加指针命中测试；关闭时不产生图层命中结果。
    pub interactive: bool,
    /// 该图层绘制期间登记的局部命中区域，仅由运行时使用。
    pub(crate) regions: Vec<crate::runtime::HitRegion>,
    /// 每种属性至多一条运动记录；缺省属性保持默认值：不透明度/缩放为 1，偏移为 0。
    pub motions: Vec<LayerMotion>,
}

/// 可由宿主设置或动画的图层属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerProperty {
    Opacity,
    OffsetX,
    OffsetY,
    ScaleX,
    ScaleY,
}

/// 属性更新命令及停止动画时的取值策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerOperation {
    Animate,
    Set,
    StopCurrent,
    StopEnd,
}

/// 单个图层属性的动画或即时更新状态。
///
/// 原生合成器持有实时呈现状态；此结构保留宿主侧重定向与恢复所需的信息，
/// `from`/`to` 的插值仅是命中测试和恢复时的近似采样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerMotion {
    /// 同一事务内显式set提供起点，不能改用原生当前值。
    pub snap_from: bool,
    /// 此记录控制的唯一图层属性。
    pub property: LayerProperty,
    /// 插值进度曲线；仅 Animate 在期限内采样时使用。
    pub easing: crate::abi::Easing,
    /// 新命令的单调修订号；帧重放不得递增，以保持事务重试语义。
    pub revision: u64,
    /// 即时设置、动画或停止策略。
    pub operation: LayerOperation,
    /// 恢复/命中测试用的近似起值；实时重定向以合成器的 StartingValue 为准。
    pub from: f32,
    /// Animate、Set、StopEnd 的目标；StopCurrent 恢复时保存采样值，实时停止则冻结原生呈现值。
    pub to: f32,
    /// 宿主单调时钟上的绝对起始时刻；场景校验拒绝未来起点。
    pub start: Instant,
    /// 宿主单调时钟上的绝对截止时刻；必须不早于起点且间隔不超过一小时。
    pub deadline: Instant,
}

impl LayerMotion {
    /// 用宿主单调时钟对运动进行近似采样。
    ///
    /// 非动画命令及截止后的采样直接返回目标值；动画按起止时刻归一化进度，
    /// 应用缓动后在线性插值。该结果服务于宿主逻辑，不替代合成器的实时呈现值。
    pub fn value_at(&self, now: Instant) -> f32 {
        if self.operation != LayerOperation::Animate || now >= self.deadline {
            return self.to;
        }
        let duration = self
            .deadline
            .saturating_duration_since(self.start)
            .as_secs_f64();
        let progress = if duration == 0.0 {
            1.0
        } else {
            (now.saturating_duration_since(self.start).as_secs_f64() / duration).clamp(0.0, 1.0)
        };
        let progress = match self.easing {
            crate::abi::Easing::Linear => progress,
            crate::abi::Easing::SmoothStep => progress * progress * (3.0 - 2.0 * progress),
            crate::abi::Easing::EaseIn => progress * progress,
            crate::abi::Easing::EaseOut => 1.0 - (1.0 - progress) * (1.0 - progress),
        };
        self.from + (self.to - self.from) * progress as f32
    }
}

impl LayerScene {
    /// 返回绘制与命中共用的从后到前顺序，不改变存储顺序或图层身份。
    ///
    /// 先按 z-index，再按创建代次排序；返回的引用仍指向原场景。
    pub fn ordered(&self) -> Vec<&LayerState> {
        let mut layers: Vec<_> = self.layers.iter().collect();
        layers.sort_by_key(|l| (l.z_index, l.generation));
        layers
    }
    /// 按视觉前景到背景的顺序执行交互命中测试。
    ///
    /// 使用宿主时钟采样动画近似逆变换指针坐标；依次应用内容坐标裁剪、
    /// 平移和缩放，再从后向前查找区域。不可逆的非正缩放不命中。命中时
    /// 返回图层 ID、创建代次和区域 ID，供调用方识别生命周期并路由事件。
    pub fn hit(&self, x: f32, y: f32, now: Instant) -> Option<(u32, u64, i32)> {
        for layer in self.ordered().into_iter().rev().filter(|l| l.interactive) {
            if layer
                .clip
                .is_some_and(|r| x < r.x || y < r.y || x >= r.x + r.w || y >= r.y + r.h)
            {
                continue;
            }
            let value = |p, default| {
                layer
                    .motions
                    .iter()
                    .find(|m| m.property == p)
                    .map_or(default, |m| m.value_at(now))
            };
            let sx = value(LayerProperty::ScaleX, 1.0);
            let sy = value(LayerProperty::ScaleY, 1.0);
            if sx <= 0.0 || sy <= 0.0 {
                continue;
            }
            let lx = (x - value(LayerProperty::OffsetX, 0.0)) / sx;
            let ly = (y - value(LayerProperty::OffsetY, 0.0)) / sy;
            if lx < 0.0 || ly < 0.0 || lx >= layer.size.0 || ly >= layer.size.1 {
                continue;
            }
            if let Some(region) = layer.regions.iter().rev().find(|r| r.contains(lx, ly)) {
                return Some((layer.id, layer.generation, region.id));
            }
        }
        None
    }
    /// 按 DPI 验证图层身份、几何、绘制栈、动画和场景级配额。
    ///
    /// 校验包括表面总像素数、命令数、命中区域数及绘制状态栈深度；替换表面
    /// 时旧表面仍可见，因此瞬时显存可能达到该场景配额的两倍。字体、图片等
    /// 资源载荷配额由运行时资源注册表负责，本方法不检查其占用。
    pub fn validate(&self, dpi: u32) -> bool {
        if dpi == 0 || self.layers.len() > MAX_LAYERS {
            return false;
        }
        let mut pixels = 0u64;
        let mut commands = 0usize;
        let now = Instant::now();
        if self.layers.iter().map(|l| l.regions.len()).sum::<usize>() > 256 {
            return false;
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.clip.is_some_and(|r| {
                ![r.x, r.y, r.w, r.h]
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 16384.0)
                    || r.w <= 0.0
                    || r.h <= 0.0
            }) {
                return false;
            }
            if layer.id == 0
                || self.layers[..index].iter().any(|l| l.id == layer.id)
                || ![layer.size.0, layer.size.1]
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= 16384.0)
                || layer.motions.len() > 5
            {
                return false;
            }
            let scale = dpi as f64 / 96.0;
            let w = (layer.size.0 as f64 * scale).ceil();
            let h = (layer.size.1 as f64 * scale).ceil();
            if w > 16384.0 || h > 16384.0 {
                return false;
            }
            pixels += w as u64 * h as u64;
            commands += layer.commands.len();
            if pixels > MAX_LAYER_PIXELS || commands > MAX_LAYER_COMMANDS {
                return false;
            }
            let mut depth = 0usize;
            for command in &layer.commands {
                if !command.is_finite() {
                    return false;
                }
                match command {
                    DrawCommand::PushClip(_) | DrawCommand::PushTransform(_) => depth += 1,
                    DrawCommand::PopState if depth == 0 => return false,
                    DrawCommand::PopState => depth -= 1,
                    _ => (),
                }
                if depth > 64 {
                    return false;
                }
            }
            if depth != 0 {
                return false;
            }
            for (index, motion) in layer.motions.iter().enumerate() {
                if layer.motions[..index]
                    .iter()
                    .any(|m| m.property == motion.property)
                    || ![motion.from, motion.to]
                        .iter()
                        .all(|v| match motion.property {
                            LayerProperty::Opacity => v.is_finite() && (0.0..=1.0).contains(v),
                            LayerProperty::OffsetX | LayerProperty::OffsetY => {
                                v.is_finite() && v.abs() <= 16384.0
                            }
                            LayerProperty::ScaleX | LayerProperty::ScaleY => {
                                v.is_finite() && (0.0..=16.0).contains(v)
                            }
                        })
                    || motion.start > now
                    || motion.deadline < motion.start
                    || motion.deadline.duration_since(motion.start) > Duration::from_secs(3600)
                {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_and_quotas() {
        let start = Instant::now();
        let motion = LayerMotion {
            snap_from: false,
            property: LayerProperty::Opacity,
            revision: 1,
            easing: crate::abi::Easing::Linear,
            operation: LayerOperation::Animate,
            from: 0.0,
            to: 1.0,
            start,
            deadline: start + Duration::from_secs(2),
        };
        assert_eq!(motion.value_at(start + Duration::from_secs(1)), 0.5);
        assert_eq!(motion.value_at(start + Duration::from_secs(4)), 1.0);
        let layer = LayerState {
            z_index: 0,
            clip: None,
            interactive: false,
            regions: Vec::new(),
            generation: 1,
            id: 1,
            size: (64.0, 64.0),
            commands: vec![],
            motions: vec![motion],
        };
        let mut scene = LayerScene {
            layers: vec![layer.clone()],
        };
        assert!(scene.validate(96));
        scene.layers.push(layer);
        assert!(!scene.validate(96));
        scene.layers.pop();
        scene.layers[0].size = (4096.0, 4096.0);
        assert!(scene.validate(96));
        assert!(!scene.validate(192));
        scene.layers[0].commands.push(DrawCommand::PopState);
        assert!(!scene.validate(96));
    }
}
