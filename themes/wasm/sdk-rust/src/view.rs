//! 同步只读快照，不经过JSON；只在主题回调中读取。数值保持原始精度。
use crate::{
    raw,
    types::{ViewField, ViewStringField},
};

pub struct Item {
    pub primary: String,
    pub secondary: String,
    pub enabled: bool,
}
pub struct Preedit {
    pub text: String,
    /// UTF-16单元偏移，不是UTF-8字节或Unicode字符数。
    pub cursor_utf16: u32,
}
pub struct View {
    pub content_id: u64,
    pub active: bool,
    pub visible: bool,
    pub ascii_mode: Option<bool>,
    pub items: Vec<Item>,
    pub selected_index: u32,
    pub page_start: u32,
    pub total_item_count: Option<u32>,
    pub can_page_previous: bool,
    pub can_page_next: bool,
    pub preedit: Option<Preedit>,
    /// 屏幕像素LTRB；主题布局仍使用DIP，不应自行换算窗口定位。
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
