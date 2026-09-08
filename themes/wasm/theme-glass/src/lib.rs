//! Aero 风格横向候选栏：WASM 负责布局与交互，宿主负责文字和毛玻璃合成。
//!
//! 当前采用固定配色，不支持候选窗内的 preedit。所有尺寸为 DIP，颜色为 ARGB；
//! 不自行处理 DPI 或创建窗口，也不填充不透明背景，以保留宿主的玻璃材质。
use std::cell::{Cell, RefCell};
use weasel_wasm_sdk::{
    ABI_VERSION, BackdropStyle, CANDIDATE, FONT_TEXT_BOLD, Kind, draw, measure, raw, send_action,
    set_backdrop, set_panel, set_size, set_text_glow,
};

const PAD: f32 = 8.0;
const PAD_Y: f32 = 4.0;
const ROW: f32 = 30.0;
const GAP: f32 = 4.0;
const TEXT_SIZE: f32 = 14.0;
const SMALL_SIZE: f32 = 14.0;
const TEXT_X: f32 = 22.0;
const COMMENT_GAP: f32 = 8.0;
const MAX_ITEMS: usize = 64;
const MAX_WIDTH: f32 = 4096.0;
const MOUSE_DOWN: i32 = 0;
const MOUSE_UP: i32 = 2;
const MOUSE_LEAVE: i32 = 3;
const ACTION_ITEM: i32 = 0;

struct Item {
    text: String,
    comment: String,
    enabled: bool,
    // 常规与粗体宽度的最大值，同时用于定位注释，避免选中项变化引发布局跳动。
    text_width: f32,
    x: f32,
    width: f32,
}

#[derive(Default)]
struct View {
    items: Vec<Item>,
    width: f32,
    selected: Option<usize>,
    pressed: Option<usize>,
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View::default());
    // draw_text 接收布局框左上角而非基线；初始化时按实际行高计算居中偏移。
    static TEXT_Y: Cell<[f32; 3]> = const { Cell::new([0.0; 3]) };
}

fn paint(v: &View) {
    if v.width == 0.0 {
        return;
    }
    set_panel(10.0, 10.0, 0.0, 2.0, 0x50002040);
    set_backdrop(&BackdropStyle {
        enabled: true,
        tint: 0xffdce8f0,
        blur_sigma: 19.0,
        backdrop_balance: 0.30,
        afterglow_balance: 0.30,
        color_balance: 0.40,
        fallback_color: 0xffdce8f0,
    });
    set_size(v.width, PAD_Y * 2.0 + ROW);
    let offsets = TEXT_Y.with(Cell::get);
    // 只提交文字，不画选中背景。玻璃参数由宿主缓存，不会每次重建效果图。
    for (i, item) in v.items.iter().enumerate() {
        let y = PAD_Y;
        let font = if v.selected == Some(i) && item.enabled {
            FONT_TEXT_BOLD
        } else {
            0
        };
        let color = if item.enabled { 0xff000000 } else { 0xff707070 };
        draw(
            &(i + 1).to_string(),
            item.x + 6.0,
            y + offsets[1],
            1,
            SMALL_SIZE,
            color,
        );
        draw(
            &item.text,
            item.x + TEXT_X,
            y + offsets[0],
            font,
            TEXT_SIZE,
            color,
        );
        draw(
            &item.comment,
            item.x + TEXT_X + COMMENT_GAP + item.text_width,
            y + offsets[2],
            2,
            SMALL_SIZE,
            color,
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn abi_version() -> i32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn probe_preedit(required: i32) -> i32 {
    // 在 init 前拒绝外置 preedit，使 renderer 可以回退到支持它的主题。
    if required == 0 { 0 } else { 1 }
}

#[unsafe(no_mangle)]
pub extern "C" fn init(_mode: i32, _dark: i32) -> i32 {
    set_text_glow(3.0, 0xffffffff);
    let mut offsets = [0.0; 3];
    for (slot, size) in [TEXT_SIZE, SMALL_SIZE, SMALL_SIZE].into_iter().enumerate() {
        let height = unsafe { raw::line_height(slot as i32, size) };
        if !height.is_finite() || height <= 0.0 || height > ROW {
            return 1;
        }
        offsets[slot] = (ROW - height) / 2.0;
    }
    TEXT_Y.with(|value| value.set(offsets));
    0
}

/// 从宿主结构化快照读取候选并完成布局；失败时不发布半成品。
fn read_view() -> Option<View> {
    if CANDIDATE.kind("") != Kind::Object
        || CANDIDATE.kind("/items") != Kind::Array
        || CANDIDATE.kind("/preedit") == Kind::Object
    {
        return None;
    }
    let count = CANDIDATE.len("/items").unwrap_or(0);
    if count > MAX_ITEMS {
        return None;
    }
    let mut v = View {
        width: PAD * 2.0,
        ..View::default()
    };
    for i in 0..count {
        let path = format!("/items/{i}");
        let text = CANDIDATE
            .string(&format!("{path}/primary_text"))
            .unwrap_or_default();
        let comment = CANDIDATE
            .string(&format!("{path}/secondary_text"))
            .unwrap_or_default();
        // 两种字重共用一个预留槽位，鼠标命中区域与绘制宽度保持一致。
        let text_width =
            measure(&text, 0, TEXT_SIZE).max(measure(&text, FONT_TEXT_BOLD, TEXT_SIZE));
        let width = TEXT_X
            + text_width
            + 6.0
            + if comment.is_empty() {
                0.0
            } else {
                COMMENT_GAP + measure(&comment, 2, SMALL_SIZE)
            };
        let x = v.width - PAD + if i == 0 { 0.0 } else { GAP };
        if !width.is_finite() || x + width + PAD > MAX_WIDTH {
            return None;
        }
        v.width = x + width + PAD;
        v.items.push(Item {
            text,
            comment,
            text_width,
            x,
            width,
            enabled: CANDIDATE
                .boolean(&format!("{path}/enabled"))
                .unwrap_or(false),
        });
    }
    v.selected = CANDIDATE
        .integer("/selected_index")
        .and_then(|i| usize::try_from(i).ok())
        .filter(|&i| i < count);
    Some(v)
}

#[unsafe(no_mangle)]
pub extern "C" fn render() -> i32 {
    // 即使新快照无效，也不能让旧候选继续响应点击。
    hide();
    let Some(v) = read_view() else {
        return 1;
    };
    paint(&v);
    VIEW.with(|state| *state.borrow_mut() = v);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn mouse(kind: i32, x: f32, y: f32) {
    VIEW.with(|state| {
        let mut v = state.borrow_mut();
        let row = if x.is_finite() && y.is_finite() && y >= PAD_Y && y < PAD_Y + ROW {
            v.items
                .iter()
                .position(|item| item.enabled && x >= item.x && x < item.x + item.width)
        } else {
            None
        };
        match kind {
            MOUSE_DOWN => v.pressed = row,
            MOUSE_UP => {
                // 仅在同一个有效候选上按下、抬起时提交；移出或刷新都会取消。
                let pressed = v.pressed.take();
                if let Some(i) = row.filter(|_| row == pressed) {
                    send_action(ACTION_ITEM, i as i32);
                }
            }
            MOUSE_LEAVE => v.pressed = None,
            _ => {}
        }
    });
}

#[unsafe(no_mangle)]
// 无动画：无需请求宿主持续调度帧。
pub extern "C" fn frame(_now: f64) {}
#[unsafe(no_mangle)]
pub extern "C" fn hide() {
    VIEW.with(|state| *state.borrow_mut() = View::default());
}
#[unsafe(no_mangle)]
pub extern "C" fn refresh(_dark: i32) {
    VIEW.with(|state| paint(&state.borrow()));
}
