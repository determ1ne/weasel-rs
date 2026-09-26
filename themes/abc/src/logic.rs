//! ABC 主题的纯几何与交互逻辑，不创建窗口或图形资源。
//!
//! 布局尺寸统一以 DIP 表示；像素换算、候选窗定位、命中测试和鼠标手势状态机
//! 集中于此，便于界面层在 UI 线程上复用且不把原生资源带入逻辑层。
use crate::theme_api::{CandidateView, UiAction};
/// 候选行高，单位为 DIP。
pub const ROW: f32 = 16.0;
/// 内容与边框之间的内边距，单位为 DIP。
pub const PAD: f32 = 4.0;
/// 预编辑输入窗宽度，单位为 DIP。
pub const INPUT_WIDTH: f32 = 173.0;
/// 预编辑输入窗高度，单位为 DIP。
pub const INPUT_HEIGHT: f32 = 26.0;
/// 候选窗固定宽度，单位为 DIP。
pub const CANDIDATE_WIDTH: f32 = 127.0;
/// 中英文模式提示窗的正方形边长，单位为 DIP。
pub const MODE_INDICATOR_SIZE: f32 = 40.0;
/// 输入窗与候选窗之间的间距，单位为 DIP。
pub const GAP: f32 = 8.0;
/// 窗口角色决定采用输入框还是候选列表布局。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 显示预编辑文本，不含候选命中区域。
    Input,
    /// 显示候选项和分页控件。
    Candidates,
    /// 在独立正方形窗口中显示短暂的中英文模式提示。
    ModeIndicator,
}

/// 将候选窗放在输入窗右侧；超出工作区右边界时改放左侧，并将坐标限制在工作区内。
///
/// `input`、`size`、`gap` 和工作区坐标均为屏幕像素。若窗口大于工作区，返回值仍
/// 尽量贴合工作区起始边界；边界运算使用饱和算术避免整数溢出。
pub fn candidate_position(
    input: (i32, i32),
    input_width: i32,
    size: (i32, i32),
    gap: i32,
    work: &crate::theme_api::Anchor,
) -> (i32, i32) {
    let right = input.0.saturating_add(input_width).saturating_add(gap);
    let x = if right.saturating_add(size.0) > work.right {
        input.0.saturating_sub(size.0).saturating_sub(gap)
    } else {
        right
    };
    (
        x.min(work.right.saturating_sub(size.0)).max(work.left),
        input
            .1
            .min(work.bottom.saturating_sub(size.1))
            .max(work.top),
    )
}

/// 布局单元的语义身份；装饰箭头和分页按钮动作相同，但按压身份独立。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 候选项索引，索引对应快照中的项目位置。
    Candidate(usize),
    /// 上一页分页按钮。
    Previous,
    /// 下一页分页按钮。
    Next,
    /// 装饰区域中的上一页按钮。
    PreviousDecorative,
    /// 装饰区域中的下一页按钮。
    NextDecorative,
}
/// 一个可绘制、可命中的矩形及其语义身份，边界使用 DIP。
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// 点击该区域对应的操作身份。
    pub hit: Hit,
    /// 左边界，包含。
    pub left: f32,
    /// 上边界，包含。
    pub top: f32,
    /// 右边界，不包含。
    pub right: f32,
    /// 下边界，不包含。
    pub bottom: f32,
}
/// 单个窗口的 DIP 布局；候选窗单元按候选顺序排列，随后是分页控件。
#[derive(Default)]
pub struct Layout {
    /// 可见控件矩形；命中测试按此顺序返回首个匹配单元。
    pub cells: Vec<Cell>,
    /// 窗口宽度，单位为 DIP。
    pub width: f32,
    /// 窗口高度，单位为 DIP。
    pub height: f32,
}
impl Layout {
    /// 按窗口角色和候选数量计算布局。
    ///
    /// 候选窗至少保留九行高度，但不会截断更多候选；输入框不创建交互单元。
    pub fn new(count: usize, role: Role) -> Self {
        match role {
            Role::Input => {
                return Self {
                    cells: Vec::new(),
                    width: INPUT_WIDTH,
                    height: INPUT_HEIGHT,
                };
            }
            Role::ModeIndicator => {
                return Self {
                    cells: Vec::new(),
                    width: MODE_INDICATOR_SIZE,
                    height: MODE_INDICATOR_SIZE,
                };
            }
            Role::Candidates => {}
        }
        let rows = count.max(9);
        let text_bottom = ROW * rows as f32 + 1.0;
        let mut cells = Vec::new();
        for i in 0..count {
            cells.push(Cell {
                hit: Hit::Candidate(i),
                left: PAD,
                top: PAD + i as f32 * ROW,
                right: 112.0,
                bottom: PAD + (i + 1) as f32 * ROW,
            });
        }
        for (hit, left) in [
            (Hit::PreviousDecorative, 4.0),
            (Hit::NextDecorative, 18.0),
            (Hit::Previous, 95.0),
            (Hit::Next, 109.0),
        ] {
            cells.push(Cell {
                hit,
                left,
                top: text_bottom + 6.0,
                right: left + 14.0,
                bottom: text_bottom + 20.0,
            });
        }
        Self {
            cells,
            width: CANDIDATE_WIDTH,
            height: text_bottom + 24.0,
        }
    }
    /// 查找包含给定 DIP 点的第一个单元；矩形左/上边界包含，右/下边界排除。
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.cells
            .iter()
            .find(|c| x >= c.left && x < c.right && y >= c.top && y < c.bottom)
            .map(|c| c.hit)
    }
}
/// 判断某控件是否可操作，同时要求快照整体可见。
///
/// 候选项索引越界时视为禁用；分页按钮依照快照的分页能力标志判断。
pub fn enabled(view: &CandidateView, hit: Hit) -> bool {
    view.visible
        && match hit {
            Hit::Candidate(i) => view.items.get(i).is_some_and(|v| v.enabled),
            Hit::Previous | Hit::PreviousDecorative => view.can_page_previous,
            Hit::Next | Hit::NextDecorative => view.can_page_next,
        }
}
/// 单次鼠标手势的按下与悬停状态。
///
/// 只有在同一控件上按下并释放、且释放时该控件仍启用，才会产生操作；离开按下
/// 控件会取消按压状态，因此拖回控件不能意外触发。
#[derive(Default)]
pub struct Gesture {
    /// 当前按下的控件身份。
    pub pressed: Option<Hit>,
    /// 当前鼠标所在控件身份。
    pub hovered: Option<Hit>,
}
impl Gesture {
    /// 清空手势状态，供失焦、捕获改变或窗口隐藏时取消操作。
    pub fn cancel(&mut self) {
        self.pressed = None;
        self.hovered = None;
    }
    /// 仅在命中控件当前启用时开始按压。
    pub fn press(&mut self, hit: Option<Hit>, view: &CandidateView) {
        self.pressed = hit.filter(|h| enabled(view, *h));
    }
    /// 更新悬停位置；指针离开原按压控件时立即取消按压。
    pub fn motion(&mut self, hit: Option<Hit>) {
        self.hovered = hit;
        if self.pressed != hit {
            self.pressed = None;
        }
    }
    /// 完成手势并映射为主题操作；释放位置不匹配或控件已禁用时返回 `None`。
    pub fn release(&mut self, hit: Option<Hit>, view: &CandidateView) -> Option<UiAction> {
        let pressed = self.pressed.take()?;
        if hit != Some(pressed) || !enabled(view, pressed) {
            return None;
        }
        Some(match pressed {
            Hit::Candidate(i) => UiAction::ItemInvoked(i as u32),
            Hit::Previous | Hit::PreviousDecorative => UiAction::NavigatePrevious,
            Hit::Next | Hit::NextDecorative => UiAction::NavigateNext,
        })
    }
}

/// 将 DIP 尺寸按 DPI 四舍五入换算为像素，并保证结果至少为 1。
pub fn pixels(dip: f32, dpi: u32) -> i32 {
    (dip * dpi.max(1) as f32 / 96.0).round().max(1.0) as i32
}
/// Direct2D 设备丢失后的有界重试状态。
///
/// 连续失败最多允许三次重建尝试；非设备丢失错误不消耗重试额度。一次成功绘制
/// 会清零失败计数，`waiting` 表示正在等待重试定时器。
#[derive(Default)]
pub struct Recovery {
    /// 连续设备丢失次数，最多递增到三。
    failures: u8,
    /// 是否等待定时器触发后重绘。
    pub waiting: bool,
}
impl Recovery {
    /// 登记一次绘图失败；仅可恢复的设备丢失且额度未耗尽时返回 `true` 并进入等待态。
    pub fn failed(&mut self, device_lost: bool) -> bool {
        if !device_lost || self.failures >= 3 {
            return false;
        }
        self.failures += 1;
        self.waiting = true;
        true
    }
    /// 绘制成功后清空失败次数与等待标志。
    pub fn succeeded(&mut self) {
        self.failures = 0;
        self.waiting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_button_pairs_page_but_do_not_share_press_identity() {
        let layout = Layout::new(9, Role::Candidates);
        let view = CandidateView {
            visible: true,
            can_page_previous: true,
            can_page_next: true,
            ..Default::default()
        };
        for (x, hit, action) in [
            (5.0, Hit::PreviousDecorative, UiAction::NavigatePrevious),
            (19.0, Hit::NextDecorative, UiAction::NavigateNext),
            (96.0, Hit::Previous, UiAction::NavigatePrevious),
            (110.0, Hit::Next, UiAction::NavigateNext),
        ] {
            assert_eq!(layout.hit(x, 152.0), Some(hit));
            let mut gesture = Gesture::default();
            gesture.press(Some(hit), &view);
            assert_eq!(gesture.release(Some(hit), &view), Some(action));
            assert!(!enabled(
                &CandidateView {
                    visible: true,
                    ..Default::default()
                },
                hit
            ));
        }
        assert_eq!(layout.hit(60.0, 152.0), None);
        let mut gesture = Gesture::default();
        gesture.press(Some(Hit::PreviousDecorative), &view);
        assert_eq!(gesture.release(Some(Hit::Previous), &view), None);
    }
    #[test]
    fn independent_window_metrics_and_candidate_rows() {
        let input = Layout::new(9, Role::Input);
        let candidates = Layout::new(9, Role::Candidates);
        let indicator = Layout::new(0, Role::ModeIndicator);
        assert_eq!((input.width, input.height), (173.0, 26.0));
        assert_eq!((candidates.width, candidates.height), (127.0, 169.0));
        assert_eq!((indicator.width, indicator.height), (40.0, 40.0));
        assert!(input.cells.is_empty());
        assert!(indicator.cells.is_empty());
        assert_eq!(candidates.hit(5.0, 5.0), Some(Hit::Candidate(0)));
        assert_eq!(candidates.hit(5.0, 21.0), Some(Hit::Candidate(1)));
        assert_eq!(candidates.hit(96.0, 152.0), Some(Hit::Previous));
    }
    #[test]
    fn page_size_is_not_silently_truncated() {
        let layout = Layout::new(10, Role::Candidates);
        assert_eq!(layout.hit(5.0, 149.0), Some(Hit::Candidate(9)));
        assert_eq!(layout.height, 185.0);
    }
    #[test]
    fn candidate_flips_left_and_clamps_on_negative_monitor() {
        let work = crate::theme_api::Anchor {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
            valid: true,
        };
        assert_eq!(
            candidate_position((-1800, 100), 173, (127, 169), 8, &work),
            (-1619, 100)
        );
        assert_eq!(
            candidate_position((-200, 1000), 173, (127, 169), 8, &work),
            (-335, 911)
        );
    }
    #[test]
    fn release_outside_cancels() {
        let view = CandidateView {
            visible: true,
            can_page_next: true,
            ..Default::default()
        };
        let mut gesture = Gesture::default();
        gesture.press(Some(Hit::Next), &view);
        gesture.motion(None);
        assert_eq!(gesture.release(Some(Hit::Next), &view), None);
        gesture.press(Some(Hit::Next), &view);
        assert_eq!(
            gesture.release(Some(Hit::Next), &view),
            Some(UiAction::NavigateNext)
        );
    }
}
