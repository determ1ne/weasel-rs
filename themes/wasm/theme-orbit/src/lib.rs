//! Orbit（月轨）：将候选布局、透明面板和原生图层动画组合成可复用主题。
//!
//! `View` 与外观事件负责读取快照、布局并提交主帧；指针事件只更新悬停或发送动作，
//! 动画事件则续接宿主合成器中的图层时间线。翻页时短暂保留旧页，新页与命中区域共用
//! 位移，旧页不可交互；装饰动画按可见时间推进，隐藏期间冻结。
mod page;
mod style;
mod text;
include!(concat!(env!("OUT_DIR"), "/theme_metadata.rs"));

use std::cell::RefCell;
use style::Style;
use weasel_wasm_sdk::{
    animation::{cancel_wakeup, request_wakeup},
    draw::{
        draw, fill_rect, line_height, measure, pop_draw_state, push_clip, rounded_rect, set_font,
    },
    interaction::{Action, PointerPhase, hit_region, pointer_layer, pointer_region, send_action},
    layers::{
        Easing, LayerProperty, layer_animate, layer_remove, layer_set, layer_z_index, with_layer,
    },
    lifecycle::{ABI_VERSION, Capability, ErrorCode, EventKind, FrameResult},
    resources::Image,
    surface::{BackdropStyle, frame_geometry, panel_bounds, set_backdrop, set_panel, set_visible},
    view::View,
};

const MOON: i32 = 1;
const STARS: i32 = 2;
const PREVIOUS: i32 = 1001;
const NEXT: i32 = 1002;
const TOP: f32 = 24.0;
const MAX_ROW: f32 = 540.0;
const PERIOD: f64 = 2200.0;
const MOTION_STEP: f64 = 100.0;

/// 跨事件保存主题资源、当前候选页和动画时间线。
///
/// 候选文字与装饰图层分开管理：普通快照可替换候选内容而不重启装饰动画；翻页只额外
/// 保留一个旧页，直到过渡截止后释放。`motion_time` 记录累计可见时长，隐藏时暂停相位。
struct State {
    style: Style,
    moon: Image,
    page: page::Page,
    transition: Option<page::Transition>,
    hover: i32,
    dark: bool,
    shown: bool,
    pressed: i32,
    next_wake: f64,
    // 只累计可见时间；隐藏时冻结相位（位置、方向和速度），不运行后台计时器。
    motion_time: f64,
    motion_started: f64,
    page_start: Option<u32>,
    palette_dirty: bool,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

/// 返回此主题实现的宿主 ABI 版本。
#[unsafe(no_mangle)]
pub extern "C" fn theme_abi_version() -> u32 {
    ABI_VERSION as u32
}
/// 声明主题使用的能力位，供宿主在创建前协商。
#[unsafe(no_mangle)]
pub extern "C" fn theme_capabilities() -> u32 {
    Capability::Preedit as u32
}

/// 初始化字体、配置和首个候选页；失败时返回对应错误码。
#[unsafe(no_mangle)]
pub extern "C" fn theme_create(_mode: i32, dark: i32) -> i32 {
    let Ok(moon) = Image::from_png(include_bytes!(concat!(env!("OUT_DIR"), "/moon.png"))) else {
        return ErrorCode::InvalidArgument as i32;
    };
    set_font(0, "Microsoft YaHei UI");
    set_font(1, "Segoe UI");
    let style = Style::read();
    let page = match page::Page::new(style.font_size) {
        Ok(text) => text,
        Err(code) => return code,
    };
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            style,
            moon,
            page,
            transition: None,
            hover: -1,
            dark: dark != 0,
            shown: false,
            pressed: -1,
            next_wake: 0.0,
            motion_time: 0.0,
            motion_started: 0.0,
            page_start: None,
            palette_dirty: true,
        })
    });
    ErrorCode::Success as i32
}

impl State {
    /// 停止唤醒、移除图层并提交透明空帧，使宿主清除旧内容和命中区域。
    fn hide(&mut self, now: f64) {
        cancel_wakeup();
        // 主动隐藏也显式移除，避免依赖宿主随后一定会发送 Hide。
        if self.shown {
            if self.style.animations {
                self.motion_time = self.phase_time(now);
            }
            for id in [MOON, STARS, page::CURRENT, page::OUTGOING] {
                layer_remove(id);
            }
        }
        self.shown = false;
        self.pressed = -1;
        self.transition = None;
        self.hover = -1;
        self.page_start = None;
        self.next_wake = 0.0;
        self.page.text.items.clear();
        self.page.cells.clear();
        self.page.enabled.clear();
        set_visible(false);
        // 含透明主命令的 Present 才能同时清空旧文字和命中区域。
        fill_rect(0.0, 0.0, 1.0, 1.0, 0);
    }

    /// 返回从本轮显示开始累计的周期相位；隐藏时保存的相位会在再次显示时续播。
    fn phase_time(&self, now: f64) -> f64 {
        (self.motion_time + (now - self.motion_started).max(0.0)) % (2.0 * PERIOD)
    }

    fn floating(time: f64) -> (f32, f32) {
        let wave = (time * std::f64::consts::PI / PERIOD).cos() as f32;
        (3.0 + 2.0 * wave, 0.625 - 0.275 * wave)
    }

    /// 为月球和星点各续接一小段原生动画，并登记最近的下一次唤醒。
    ///
    /// 这里只更新图层属性，不重绘候选文字；延迟唤醒也只安排一段新动画，不追赶漏掉的帧。
    fn animate(&mut self, now: f64) -> bool {
        if !self.shown || !self.style.animations {
            return false;
        }
        let changed = now >= self.next_wake;
        if now >= self.next_wake {
            // 原生合成器插值短线段；相位来自可见时间而非 View 次数。
            // 100ms 一次仅改图层，不重绘文字；延迟唤醒不补播积压帧。
            let (y, opacity) = Self::floating(self.phase_time(now) + MOTION_STEP);
            layer_animate(MOON, LayerProperty::OffsetY, y, MOTION_STEP, Easing::Linear);
            layer_animate(
                STARS,
                LayerProperty::Opacity,
                opacity,
                MOTION_STEP,
                Easing::Linear,
            );
            // 延迟唤醒时只创建一段新动画，不补播错过的周期。
            self.next_wake = now + MOTION_STEP;
        }
        request_wakeup(self.next_wake);
        if let Some(t) = &self.transition {
            // 唤醒请求只保留最早一次；月球唤醒后须重新登记翻页清理截止时间。
            request_wakeup(t.start + page::DURATION);
        }
        changed
    }

    /// 读取最新快照并绘制面板、候选页和装饰层；必要时启动或回收翻页过渡。
    ///
    /// 页面变化时先保留旧页，再按新布局确定整行位移并启动两层动画。后续普通快照仅更新
    /// 两页内容，不重置动画起点；过渡结束后旧页释放，新的当前页恢复静止位置。
    fn render(&mut self, now: f64) -> Result<(), i32> {
        let Some(view) = View::read() else {
            self.hide(now);
            return Ok(());
        };
        if !view.visible || (view.items.is_empty() && view.preedit.is_none()) {
            self.hide(now);
            return Ok(());
        }
        if view.items.len() > 64 {
            return Err(ErrorCode::InvalidArgument as i32);
        }
        let changed_page =
            self.style.animations && self.page_start.is_some_and(|old| old != view.page_start);
        if changed_page {
            layer_remove(page::OUTGOING);
            layer_remove(page::CURRENT);
            let (offset, opacity) = self
                .transition
                .as_ref()
                .map_or((0.0, 1.0), |t| t.incoming(now));
            let old = std::mem::replace(&mut self.page, page::Page::new(self.style.font_size)?);
            self.transition = Some(page::Transition {
                old,
                start: now,
                direction: if view.page_start > self.page_start.unwrap_or(0) {
                    1.0
                } else {
                    -1.0
                },
                old_offset: offset,
                old_opacity: opacity,
                distance: 0.0, // 新页布局确定后填写，后续 View 不修改动画跨度。
            });
            self.hover = -1;
        }
        if self
            .transition
            .as_ref()
            .is_some_and(|t| now >= t.start + page::DURATION)
        {
            self.transition = None;
            layer_remove(page::OUTGOING);
        }
        self.page.text.update(&view.items)?;
        let size = self.style.font_size;
        let small = (size - 3.0).max(11.0);
        let text_height = line_height(0, size);
        let row = text_height.max(line_height(1, small)) + 10.0;
        if !row.is_finite() || row > 100.0 {
            return Err(ErrorCode::InvalidArgument as i32);
        }
        let pre_height = if view.preedit.is_some() { row } else { 0.0 };
        // 候选按行折返，长单项裁剪；所有候选仍然保有独立命中区域。
        let mut cells = Vec::with_capacity(view.items.len());
        let (mut x, mut y, mut used) = (10.0f32, TOP + pre_height + 3.0, 0.0f32);
        for (item, text) in view.items.iter().zip(&self.page.text.items) {
            let width = (text.primary.layout.width()
                + if item.secondary.is_empty() {
                    0.0
                } else {
                    10.0 + text.secondary.layout.width().min(130.0)
                }
                + 40.0)
                .clamp(70.0, MAX_ROW);
            if x > 10.0 && x + width > MAX_ROW + 10.0 {
                x = 10.0;
                y += row;
            }
            cells.push([x, y, width, row]);
            x += width + 4.0;
            used = used.max(x);
        }
        let pre_width = view
            .preedit
            .as_ref()
            .map_or(0.0, |p| measure(&p.text, 0, size).min(MAX_ROW) + 24.0);
        let width = (used.max(pre_width) + 54.0).max(260.0);
        let paging = view.can_page_previous || view.can_page_next;
        let bottom = if cells.is_empty() {
            TOP + pre_height + 6.0
        } else {
            y + row + 3.0
        };
        let height = bottom - TOP + if paging { 22.0 } else { 0.0 };
        // 定位以面板为准；月球可以探出面板，但不超出透明内容区域。
        frame_geometry(
            width + 34.0,
            (TOP + height).max(72.0),
            [0.0, TOP, width, height],
        );
        panel_bounds([0.0, TOP, width, height]);
        set_panel(14.0, 12.0, 0.0, 3.0, 0x35081528);
        set_visible(true);
        let p = self.style.palette(self.dark);
        // 复用宿主玻璃材质，只影响 panel_bounds；不对文字/装饰整体降透明度。
        // 不再用不透明矩形覆盖材质，不支持背景采样时由宿主回退到实色。
        set_backdrop(&BackdropStyle {
            enabled: true,
            tint: p.background,
            blur_sigma: 19.0,
            backdrop_balance: 0.25,
            afterglow_balance: 0.15,
            color_balance: 0.60,
            fallback_color: p.background,
        });
        // 即使没有 preedit/分页按钮，也明确提交主帧和新快照身份。
        fill_rect(0.0, TOP, width, height, 0);
        if let Some(preedit) = &view.preedit {
            push_clip([12.0, TOP + 4.0, width - 70.0, pre_height - 4.0]);
            let py = TOP + (pre_height - text_height) / 2.0;
            draw(&preedit.text, 14.0, py, 0, size, p.text);
            // cursor_utf16 以UTF-16单元计；不能当UTF-8下标截取中文或emoji。
            let mut units = 0;
            let prefix: String = preedit
                .text
                .chars()
                .take_while(|c| {
                    units += c.len_utf16() as u32;
                    units <= preedit.cursor_utf16
                })
                .collect();
            let cx = 14.0 + measure(&prefix, 0, size);
            fill_rect(cx, py + 2.0, 1.5, text_height - 4.0, p.accent);
            pop_draw_state();
            fill_rect(12.0, TOP + pre_height, width - 72.0, 1.0, p.border);
        }
        self.page.cells = cells;
        self.page.enabled = view.items.iter().map(|item| item.enabled).collect();
        self.page.selected = view.selected_index as usize;
        let bounds = [
            8.0,
            TOP + pre_height + 2.0,
            width - 16.0,
            (bottom - (TOP + pre_height + 2.0)).max(1.0),
        ];
        if changed_page {
            let t = self.transition.as_mut().unwrap();
            let old_width = t
                .old
                .cells
                .iter()
                .map(|c| c[0] + c[2] - bounds[0])
                .fold(0.0f32, f32::max);
            t.distance = bounds[2].max(old_width) + t.old_offset.abs();
        }
        // 只替换内容，不在普通 View/hover 中重新启动原生动画。
        if let Some(t) = &self.transition {
            t.old.paint(page::OUTGOING, bounds, size, &p, -1);
        }
        self.page.paint(page::CURRENT, bounds, size, &p, self.hover);
        if changed_page {
            let t = self.transition.as_ref().unwrap();
            layer_set(page::OUTGOING, LayerProperty::OffsetX, t.old_offset);
            layer_set(page::OUTGOING, LayerProperty::Opacity, t.old_opacity);
            layer_animate(
                page::OUTGOING,
                LayerProperty::OffsetX,
                t.old_offset - t.direction * t.distance,
                page::DURATION,
                Easing::EaseOut,
            );
            layer_set(
                page::CURRENT,
                LayerProperty::OffsetX,
                t.direction * t.distance,
            );
            layer_set(page::CURRENT, LayerProperty::Opacity, 1.0);
            layer_animate(
                page::CURRENT,
                LayerProperty::OffsetX,
                0.0,
                page::DURATION,
                Easing::EaseOut,
            );
        } else if self.transition.is_none() {
            layer_set(page::CURRENT, LayerProperty::OffsetX, 0.0);
            layer_set(page::CURRENT, LayerProperty::Opacity, 1.0);
        }
        if let Some(t) = &self.transition {
            request_wakeup(t.start + page::DURATION);
        }
        if paging {
            for (id, label, enabled, x) in [
                (PREVIOUS, "‹", view.can_page_previous, width - 78.0),
                (NEXT, "›", view.can_page_next, width - 48.0),
            ] {
                draw(
                    label,
                    x + 8.0,
                    bottom + (22.0 - line_height(1, 18.0)) / 2.0,
                    1,
                    18.0,
                    if enabled { p.accent } else { p.muted },
                );
                if enabled {
                    hit_region(id, x, bottom, 26.0, 22.0, 6.0);
                }
            }
        }
        let entering = !self.shown;
        self.page_start = Some(view.page_start);
        if entering || self.palette_dirty {
            with_layer(MOON, 68.0, 68.0, || {
                self.moon.draw(0.0, 0.0, 68.0, 68.0, 1.0)
            });
            with_layer(STARS, 78.0, 62.0, || {
                for (x, y, r) in [(4.0, 13.0, 2.0), (67.0, 8.0, 1.5), (72.0, 49.0, 1.0)] {
                    rounded_rect(x - r, y - r, r * 2.0, r * 2.0, r, p.accent);
                }
            });
            self.palette_dirty = false;
        }
        // 大小变化时只挪横向挂点，不干扰纵向浮动时间线。
        // 候选页重建不会盖住月球；只改变层级，不重建装饰或重启动画。
        layer_z_index(MOON, 20);
        layer_z_index(STARS, 30);
        layer_set(MOON, LayerProperty::OffsetX, width - 36.0);
        layer_set(STARS, LayerProperty::OffsetX, width - 43.0);
        if entering {
            self.motion_started = now;
            let (y, opacity) = Self::floating(self.motion_time);
            layer_set(MOON, LayerProperty::OffsetY, y);
            layer_set(STARS, LayerProperty::Opacity, opacity);
            layer_set(MOON, LayerProperty::Opacity, 1.0);
            self.next_wake = now;
        }
        self.shown = true;
        self.animate(now);
        Ok(())
    }
}

/// 按宿主事件推进主题状态，并返回是否提交帧或保留现有画面。
///
/// `View`/外观事件重绘最新快照，指针事件处理命中与动作，动画事件续约装饰唤醒或清理已
/// 到期的退场页；隐藏事件则清除图层和画面。只有需要更新主画面或命中快照时才返回
/// `Present`，纯图层续播返回 `Keep`。
#[unsafe(no_mangle)]
pub extern "C" fn theme_event(kind: i32, detail: i32, _x: f32, _y: f32, now: f64) -> i32 {
    STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return ErrorCode::InvalidArgument as i32;
        };
        match EventKind::try_from(kind) {
            Ok(EventKind::Hide) => {
                state.hide(now);
                FrameResult::Present as i32
            }
            Ok(EventKind::Pointer) => {
                let region = pointer_region();
                let previous_hover = state.hover;
                state.hover = match PointerPhase::try_from(detail) {
                    Ok(PointerPhase::Leave | PointerPhase::Cancel) => -1,
                    _ if pointer_layer() == page::CURRENT && region > 0 && region < PREVIOUS => {
                        region
                    }
                    _ => -1,
                };
                match PointerPhase::try_from(detail) {
                    Ok(PointerPhase::Down) => state.pressed = region,
                    Ok(PointerPhase::Up) => {
                        if state.shown && region > 0 && state.pressed == region {
                            match region {
                                PREVIOUS => send_action(Action::Previous as i32, 0),
                                NEXT => send_action(Action::Next as i32, 0),
                                n => send_action(Action::Item as i32, n - 1),
                            }
                        }
                        state.pressed = -1;
                    }
                    Ok(PointerPhase::Cancel | PointerPhase::Leave) => state.pressed = -1,
                    _ => {}
                }
                if state.hover != previous_hover && state.shown {
                    state
                        .render(now)
                        .map_or_else(|e| e, |_| FrameResult::Present as i32)
                } else {
                    FrameResult::Keep as i32
                }
            }
            Ok(EventKind::Animation) => {
                if !state.shown || !state.style.animations {
                    return FrameResult::Keep as i32;
                }
                // 提前到达的唤醒只续约；空 Present 会清掉主画面，必须 Keep。
                if state
                    .transition
                    .as_ref()
                    .is_some_and(|t| now >= t.start + page::DURATION)
                {
                    return state
                        .render(now)
                        .map_or_else(|e| e, |_| FrameResult::Present as i32);
                }
                if state.animate(now) {
                    FrameResult::Present as i32
                } else {
                    FrameResult::Keep as i32
                }
            }
            Ok(event @ (EventKind::View | EventKind::Appearance)) => {
                if event == EventKind::Appearance {
                    state.dark = detail != 0;
                    state.palette_dirty = true;
                }
                // 新快照到达时废弃尚未释放的点击，不允许提交旧候选索引。
                state.pressed = -1;
                state.hover = -1;
                state
                    .render(now)
                    .map_or_else(|e| e, |_| FrameResult::Present as i32)
            }
            Err(_) => ErrorCode::InvalidArgument as i32,
        }
    })
}

/// 释放主题状态及其缓存资源。
#[unsafe(no_mangle)]
pub extern "C" fn theme_destroy() {
    STATE.with(|state| *state.borrow_mut() = None);
}
