//! Aero 风格的横向候选栏：本模块负责布局和鼠标交互，宿主负责文字绘制与毛玻璃合成。
//!
//! 当前采用固定配色，不支持候选窗内的 preedit。所有尺寸为 DIP，颜色为 ARGB；
//! 不自行处理 DPI 或创建窗口，也不填充不透明背景，以保留宿主的玻璃材质。候选窗口不
//! 支持预编辑文本；无效或过大的快照会被拒绝，且不会保留上一帧可点击的候选。
use std::cell::{Cell, RefCell};
use weasel_wasm_sdk::{
    ABI_VERSION, Action, BackdropStyle, ErrorCode, EventKind, FrameResult, PointerPhase,
    draw_text_glow, line_height, measure_text, send_action, set_backdrop, set_font, set_panel,
    set_size,
};
use weasel_wasm_sdk::draw::FontSlot;

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
const MOUSE_DOWN: i32 = PointerPhase::Down as i32;
const MOUSE_UP: i32 = PointerPhase::Up as i32;
const MOUSE_LEAVE: i32 = PointerPhase::Leave as i32;
const MOUSE_CANCEL: i32 = PointerPhase::Cancel as i32;
const ACTION_ITEM: i32 = Action::Item as i32;

struct Item {
    text: String,
    comment: String,
    enabled: bool,
    // 常规与粗体宽度的最大值，同时用于定位注释，避免选中项变化引发布局跳动。
    text_width: f32,
    x: f32,
    width: f32,
}

#[derive(Clone, Copy)]
struct Fonts {
    text: FontSlot,
    number: FontSlot,
    comment: FontSlot,
    bold: FontSlot,
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
    static FONTS: Cell<Option<Fonts>> = const { Cell::new(None) };
    // draw_text 接收布局框左上角而非基线；初始化时按实际行高计算居中偏移。
    static TEXT_Y: Cell<[f32; 3]> = const { Cell::new([0.0; 3]) };
}

fn paint(v: &View) {
    paint_body(v);
}
fn paint_body(v: &View) {
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
    let fonts = FONTS.with(Cell::get).expect("theme fonts must be initialized");
    // 只提交文字，不画选中背景。玻璃参数由宿主缓存，不会每次重建效果图。
    for (i, item) in v.items.iter().enumerate() {
        let y = PAD_Y;
        let font = if v.selected == Some(i) && item.enabled {
            fonts.bold
        } else {
            fonts.text
        };
        let color = if item.enabled { 0xff000000 } else { 0xff707070 };
        draw_text_glow(
            fonts.number,
            &(i + 1).to_string(),
            item.x + 6.0,
            y + offsets[1],
            SMALL_SIZE,
            color,
            3.0,
            0xffffffff,
        );
        draw_text_glow(
            font,
            &item.text,
            item.x + TEXT_X,
            y + offsets[0],
            TEXT_SIZE,
            color,
            3.0,
            0xffffffff,
        );
        draw_text_glow(
            fonts.comment,
            &item.comment,
            item.x + TEXT_X + COMMENT_GAP + item.text_width,
            y + offsets[2],
            SMALL_SIZE,
            color,
            3.0,
            0xffffffff,
        );
    }
}

/// 返回此模块实现的主题 ABI 版本，供宿主在调用其他入口前协商接口。
#[unsafe(no_mangle)]
pub extern "C" fn theme_abi_version() -> u32 {
    ABI_VERSION as u32
}
/// 返回主题能力位；当前实现不声明额外能力。
#[unsafe(no_mangle)]
pub extern "C" fn theme_capabilities() -> u32 {
    0
}

/// 初始化主题的行高布局状态；返回零表示成功，非零值为 ABI 错误码。
#[unsafe(no_mangle)]
pub extern "C" fn theme_create(_mode: i32, _dark: i32) -> i32 {
    let fonts = Fonts {
        text: set_font(0, "Microsoft YaHei UI", 400),
        number: set_font(1, "Segoe UI", 400),
        comment: set_font(2, "Microsoft YaHei UI", 400),
        bold: set_font(3, "Microsoft YaHei UI", 700),
    };
    let mut offsets = [0.0; 3];
    for (index, (font, size)) in [
        (fonts.text, TEXT_SIZE),
        (fonts.number, SMALL_SIZE),
        (fonts.comment, SMALL_SIZE),
    ]
    .into_iter()
    .enumerate()
    {
        let height = line_height(font, size);
        if !height.is_finite() || height <= 0.0 || height > ROW {
            return ErrorCode::InvalidArgument as i32;
        }
        offsets[index] = (ROW - height) / 2.0;
    }
    FONTS.with(|value| value.set(Some(fonts)));
    TEXT_Y.with(|value| value.set(offsets));
    0
}

/// 从宿主结构化快照读取候选并完成布局；失败时不发布半成品。
/// 候选文本由新建的 `String` 持有；超出数量、尺寸限制或包含预编辑内容时返回 `None`。
fn read_view() -> Option<View> {
    let snapshot = weasel_wasm_sdk::View::read()?;
    if snapshot.preedit.is_some() {
        return None;
    }
    let count = snapshot.items.len();
    if count > MAX_ITEMS {
        return None;
    }
    let mut v = View {
        width: PAD * 2.0,
        ..View::default()
    };
    let fonts = FONTS.with(Cell::get)?;
    for (i, item) in snapshot.items.into_iter().enumerate() {
        let text = item.primary;
        let comment = item.secondary;
        // 两种字重共用一个预留槽位，鼠标命中区域与绘制宽度保持一致。
        let text_width =
            measure_text(fonts.text, &text, TEXT_SIZE)
                .max(measure_text(fonts.bold, &text, TEXT_SIZE));
        let width = TEXT_X
            + text_width
            + 6.0
            + if comment.is_empty() {
                0.0
            } else {
                COMMENT_GAP + measure_text(fonts.comment, &comment, SMALL_SIZE)
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
            enabled: item.enabled,
        });
    }
    v.selected = Some(snapshot.selected_index as usize).filter(|&i| i < count);
    Some(v)
}

fn render() -> i32 {
    // 即使新快照无效，也不能让旧候选继续响应点击。
    hide();
    let Some(v) = read_view() else {
        return ErrorCode::InvalidArgument as i32;
    };
    paint(&v);
    VIEW.with(|state| *state.borrow_mut() = v);
    FrameResult::Present as i32
}

fn mouse(kind: i32, x: f32, y: f32) {
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
            MOUSE_LEAVE | MOUSE_CANCEL => v.pressed = None,
            _ => {}
        }
    });
}

// 无动画：无需请求宿主持续调度帧。
fn frame(_now: f64) {}
fn hide() {
    VIEW.with(|state| *state.borrow_mut() = View::default());
}
fn refresh(_dark: i32) {
    VIEW.with(|state| paint(&state.borrow()));
}

/// ABI 2 统一事件入口，处理快照、外观、隐藏、指针和动画事件。
///
/// 返回 ABI 错误码表示事件无效或快照无法接受；成功更新画面时返回 `Present`，仅改变
/// 内部交互状态时返回 `Keep`，宿主据此决定是否提交新帧。
#[unsafe(no_mangle)]
pub extern "C" fn theme_event(kind: i32, detail: i32, x: f32, y: f32, now: f64) -> i32 {
    let Ok(kind) = EventKind::try_from(kind) else {
        return ErrorCode::InvalidArgument as i32;
    };
    match kind {
        EventKind::View => return render(),
        EventKind::Appearance => {
            refresh(detail);
            return FrameResult::Present as i32;
        }
        EventKind::Hide => hide(),
        EventKind::Pointer => mouse(detail, x, y),
        EventKind::Animation => frame(now),
    }
    FrameResult::Keep as i32
}
