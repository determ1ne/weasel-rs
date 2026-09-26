//! 在主题事件回调中同步读取宿主提供的只读输入上下文快照。
//!
//! 快照不是 JSON，也不允许主题修改宿主候选数据。只有在快照存在且所有必需字符串都能
//! 成功复制时，[`View::read`] 才返回完整拥有所有权的 `View`；无快照或数据无效时返回
//! `None`。字符串在读取时复制，视图值可在当前回调中安全保存为主题自己的状态。
use crate::{
    raw,
    types::{ModeIndicatorReason, ViewField, ViewStringField},
};

/// 当前候选页中的一个条目；索引是页内索引，动作提交时绑定已展示快照。
pub struct Item {
    /// 候选的主显示文本，拥有所有权且为 UTF-8 解码后的 `String`。
    pub primary: String,
    /// 候选的辅助文本，可能为空。
    pub secondary: String,
    /// 候选是否可执行；不可用项仍可能出现在快照中。
    pub enabled: bool,
}
/// 输入预编辑文本和插入光标位置。
pub struct Preedit {
    /// 预编辑字符串，拥有所有权。
    pub text: String,
    /// 光标相对 `text` 起点的 UTF-16 代码单元偏移，不是 UTF-8 字节数或字符数。
    pub cursor_utf16: u32,
}
/// 一次由宿主定位并统一计时的短暂中英文模式提示。
pub struct ModeIndicator {
    /// 不透明提示 ID；同一 ID 的几何刷新属于同一次展示。
    pub id: u64,
    /// `true` 为英文，`false` 为中文。
    pub ascii_mode: bool,
    /// 提示由焦点进入还是用户主动切换触发。
    pub reason: ModeIndicatorReason,
}
/// 一次事件读取到的完整输入视图快照。
pub struct View {
    /// 宿主快照内容标识，按 `u64` 位模式保留。
    pub content_id: u64,
    /// 输入上下文当前是否激活。
    pub active: bool,
    /// 宿主视图当前是否可见。
    pub visible: bool,
    /// ASCII 模式状态；宿主未知时为 `None`。
    pub ascii_mode: Option<bool>,
    /// 需要在插入点附近显示的短暂模式提示。
    pub mode_indicator: Option<ModeIndicator>,
    /// 当前候选页条目；长度可能为零。
    pub items: Vec<Item>,
    /// 当前页中已选候选的页内索引。
    pub selected_index: u32,
    /// 当前页在总候选序列中的起始索引。
    pub page_start: u32,
    /// 总候选数；宿主未知时为 `None`。
    pub total_item_count: Option<u32>,
    /// 前一页是否可用。
    pub can_page_previous: bool,
    /// 后一页是否可用。
    pub can_page_next: bool,
    /// 存在预编辑内容时提供其文本和 UTF-16 光标。
    pub preedit: Option<Preedit>,
    /// 屏幕像素坐标的 `[left, top, right, bottom]` 锚点；仅供显示参考，主题布局使用 DIP，
    /// 不应据此自行换算或设置窗口位置。
    pub anchor: Option<[i32; 4]>,
}
fn integer(field: ViewField) -> i64 {
    unsafe { raw::view_i64(field as i32, 0) }
}
fn text(field: ViewStringField, index: i32) -> Option<String> {
    let n = unsafe { raw::view_string(field as i32, index, std::ptr::null_mut(), 0) };
    if !(0..=1024 * 1024).contains(&n) {
        return None;
    }
    let mut bytes = vec![0; n as usize];
    if unsafe { raw::view_string(field as i32, index, bytes.as_mut_ptr(), n) } != n {
        return None;
    }
    String::from_utf8(bytes).ok()
}
impl View {
    /// 从当前回调的宿主快照构造拥有所有权的视图。
    ///
    /// 无快照、候选数量超过 4096、字符串超过 1 MiB、UTF-8 无效或读取长度不一致时返回
    /// `None`。没有候选内容是合法状态，应检查 `items` 是否为空后再访问索引。
    pub fn read() -> Option<Self> {
        if integer(ViewField::HasSnapshot) == 0 {
            return None;
        }
        let count = integer(ViewField::ItemCount);
        if !(0..=4096).contains(&count) {
            return None;
        }
        let mut items = Vec::with_capacity(count as usize);
        for i in 0..count as i32 {
            items.push(Item {
                primary: text(ViewStringField::Primary, i)?,
                secondary: text(ViewStringField::Secondary, i)?,
                enabled: unsafe { raw::view_i64(ViewField::ItemEnabled as i32, i) != 0 },
            });
        }
        Some(Self {
            content_id: integer(ViewField::ContentId) as u64,
            active: integer(ViewField::Active) != 0,
            visible: integer(ViewField::Visible) != 0,
            ascii_mode: match integer(ViewField::AsciiMode) {
                -1 => None,
                v => Some(v != 0),
            },
            mode_indicator: if integer(ViewField::HasModeIndicator) != 0 {
                Some(ModeIndicator {
                    id: integer(ViewField::ModeIndicatorId) as u64,
                    ascii_mode: integer(ViewField::ModeIndicatorAscii) != 0,
                    reason: ModeIndicatorReason::try_from(
                        integer(ViewField::ModeIndicatorReason) as i32,
                    )
                    .ok()?,
                })
            } else {
                None
            },
            items,
            selected_index: integer(ViewField::SelectedIndex) as u32,
            page_start: integer(ViewField::PageStart) as u32,
            total_item_count: u32::try_from(integer(ViewField::TotalItemCount)).ok(),
            can_page_previous: integer(ViewField::CanPagePrevious) != 0,
            can_page_next: integer(ViewField::CanPageNext) != 0,
            preedit: if integer(ViewField::HasPreedit) != 0 {
                Some(Preedit {
                    text: text(ViewStringField::Preedit, 0)?,
                    cursor_utf16: integer(ViewField::CursorUtf16) as u32,
                })
            } else {
                None
            },
            anchor: if integer(ViewField::AnchorValid) != 0 {
                Some([
                    integer(ViewField::AnchorLeft) as i32,
                    integer(ViewField::AnchorTop) as i32,
                    integer(ViewField::AnchorRight) as i32,
                    integer(ViewField::AnchorBottom) as i32,
                ])
            } else {
                None
            },
        })
    }
}
