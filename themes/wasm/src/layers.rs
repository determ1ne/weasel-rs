//! Native retained decoration contract. Runtime owns transactional scene snapshots.
//! IDs are positive and stable; z-index then creation order sorts layers above main content.
//! Commands use layer-local DIPs; offsets are relative to the content origin.
use crate::protocol::DrawCommand;
use std::time::{Duration, Instant};

pub const MAX_LAYERS: usize = 8;
pub const MAX_LAYER_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_LAYER_COMMANDS: usize = 8192;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerScene {
    pub layers: Vec<LayerState>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerState {
    /// 每次创建的新身份，防止复用ID继承旧表面和动画。
    pub generation: u64,
    pub id: u32,
    /// 越大越靠前；相同值按创建顺序。仅在保留图层之间排序，均在主画面之上。
    pub z_index: i32,
    pub size: (f32, f32),
    pub commands: Vec<DrawCommand>,
    /// 固定在内容坐标系，不随本层位移/缩放移动。
    pub clip: Option<crate::protocol::Rect>,
    pub interactive: bool,
    pub(crate) regions: Vec<crate::runtime::HitRegion>,
    /// At most one entry per property. Omitted properties retain their value;
    /// new layers start at opacity/scale 1 and offset 0.
    pub motions: Vec<LayerMotion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerProperty {
    Opacity,
    OffsetX,
    OffsetY,
    ScaleX,
    ScaleY,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerOperation {
    Animate,
    Set,
    StopCurrent,
    StopEnd,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerMotion {
    /// 同一事务内显式set提供起点，不能改用原生当前值。
    pub snap_from: bool,
    pub property: LayerProperty,
    pub easing: crate::abi::Easing,
    /// Change this only for a new command, not on frame replay.
    pub revision: u64,
    pub operation: LayerOperation,
    /// Recovery approximation; live retargeting uses compositor StartingValue.
    pub from: f32,
    /// Target for Animate/Set/StopEnd; runtime's sampled current value for
    /// StopCurrent recovery (live StopCurrent freezes the native presentation).
    pub to: f32,
    /// Absolute host monotonic instants. Future starts are rejected.
    pub start: Instant,
    pub deadline: Instant,
}

impl LayerMotion {
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
    /// 绘制与命中共用的从后到前顺序；不改变存储顺序或图层身份。
    pub fn ordered(&self) -> Vec<&LayerState> {
        let mut layers: Vec<_> = self.layers.iter().collect();
        layers.sort_by_key(|l| (l.z_index, l.generation));
        layers
    }
    /// 使用宿主单调时钟近似合成器当前位置，按视觉顺序逆序命中。
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
    /// Surface quota is aggregate (64 MiB BGRA); replacement can temporarily
    /// double it while old surfaces remain visible. Resource payload quotas
    /// remain the runtime resource registry's responsibility.
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
