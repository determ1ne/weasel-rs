// Generated from themes/wasm/abi.json. DO NOT EDIT.
#![allow(dead_code)]
//! 与宿主 ABI 对齐的版本号、字段编号和语义枚举。
//!
//! 这些枚举和位集合的整数值属于 ABI 合约；主题应使用命名项而非自行假定编号。
/// 当前主题 ABI 版本；导出 `theme_abi_version` 时返回此值。
pub const ABI_VERSION: i32 = 2;
/// 只读快照整数属性。ItemEnabled使用候选index，其余index必须为0。AsciiMode/TotalItemCount的-1表示未知。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewField {
    ContentId = 0,
    Active = 1,
    Visible = 2,
    AsciiMode = 3,
    ItemCount = 4,
    SelectedIndex = 5,
    PageStart = 6,
    TotalItemCount = 7,
    CanPagePrevious = 8,
    CanPageNext = 9,
    HasPreedit = 10,
    CursorUtf16 = 11,
    HasSnapshot = 12,
    ItemEnabled = 13,
    AnchorValid = 14,
    AnchorLeft = 15,
    AnchorTop = 16,
    AnchorRight = 17,
    AnchorBottom = 18,
}
impl TryFrom<i32> for ViewField {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::ContentId),
            1 => Ok(Self::Active),
            2 => Ok(Self::Visible),
            3 => Ok(Self::AsciiMode),
            4 => Ok(Self::ItemCount),
            5 => Ok(Self::SelectedIndex),
            6 => Ok(Self::PageStart),
            7 => Ok(Self::TotalItemCount),
            8 => Ok(Self::CanPagePrevious),
            9 => Ok(Self::CanPageNext),
            10 => Ok(Self::HasPreedit),
            11 => Ok(Self::CursorUtf16),
            12 => Ok(Self::HasSnapshot),
            13 => Ok(Self::ItemEnabled),
            14 => Ok(Self::AnchorValid),
            15 => Ok(Self::AnchorLeft),
            16 => Ok(Self::AnchorTop),
            17 => Ok(Self::AnchorRight),
            18 => Ok(Self::AnchorBottom),
            _ => Err(value),
        }
    }
}

/// 只读快照字符串属性。Primary/Secondary使用候选index，Preedit的index必须为0。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewStringField {
    Primary = 0,
    Secondary = 1,
    Preedit = 2,
}
impl TryFrom<i32> for ViewStringField {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Primary),
            1 => Ok(Self::Secondary),
            2 => Ok(Self::Preedit),
            _ => Err(value),
        }
    }
}

/// 只暴露模块配置与允许读取的全局展示设置，不包含其他应用配置。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    Module = 1,
    Global = 2,
}
impl TryFrom<i32> for ConfigScope {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            1 => Ok(Self::Module),
            2 => Ok(Self::Global),
            _ => Err(value),
        }
    }
}

/// DataKind
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    Missing = 0,
    Null = 1,
    Bool = 2,
    Number = 3,
    String = 4,
    Array = 5,
    Object = 6,
}
impl TryFrom<i32> for DataKind {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Missing),
            1 => Ok(Self::Null),
            2 => Ok(Self::Bool),
            3 => Ok(Self::Number),
            4 => Ok(Self::String),
            5 => Ok(Self::Array),
            6 => Ok(Self::Object),
            _ => Err(value),
        }
    }
}

/// 资源度量，单位DIP；Baseline仅适用于文本布局。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceMetric {
    Width = 0,
    Height = 1,
    Baseline = 2,
}
impl TryFrom<i32> for ResourceMetric {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Width),
            1 => Ok(Self::Height),
            2 => Ok(Self::Baseline),
            _ => Err(value),
        }
    }
}

/// host到guest的统一事件种类。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    View = 0,
    Appearance = 1,
    Hide = 2,
    Pointer = 3,
    Animation = 4,
}
impl TryFrom<i32> for EventKind {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::View),
            1 => Ok(Self::Appearance),
            2 => Ok(Self::Hide),
            3 => Ok(Self::Pointer),
            4 => Ok(Self::Animation),
            _ => Err(value),
        }
    }
}

/// 取消和离开不能提交点击；取消表示捕获丢失或宿主取消。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down = 0,
    Move = 1,
    Up = 2,
    Leave = 3,
    Cancel = 4,
}
impl TryFrom<i32> for PointerPhase {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Down),
            1 => Ok(Self::Move),
            2 => Ok(Self::Up),
            3 => Ok(Self::Leave),
            4 => Ok(Self::Cancel),
            _ => Err(value),
        }
    }
}

/// Action
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Item = 0,
    Previous = 1,
    Next = 2,
    Emoji = 3,
    Dismiss = 4,
}
impl TryFrom<i32> for Action {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Item),
            1 => Ok(Self::Previous),
            2 => Ok(Self::Next),
            3 => Ok(Self::Emoji),
            4 => Ok(Self::Dismiss),
            _ => Err(value),
        }
    }
}

/// 事件成功返回值。Keep丢弃本次画面修改，Present替换整帧（可为空）；失败返回负ErrorCode。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameResult {
    Keep = 0,
    Present = 1,
}
impl TryFrom<i32> for FrameResult {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Keep),
            1 => Ok(Self::Present),
            _ => Err(value),
        }
    }
}

/// 创建/资源操作：0成功或正句柄，负数失败。事件只允许FrameResult或负错误码。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Success = 0,
    NotFound = -1,
    InvalidArgument = -2,
    InvalidHandle = -3,
    ResourceLimit = -6,
    Internal = -7,
}
impl TryFrom<i32> for ErrorCode {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Success),
            -1 => Ok(Self::NotFound),
            -2 => Ok(Self::InvalidArgument),
            -3 => Ok(Self::InvalidHandle),
            -6 => Ok(Self::ResourceLimit),
            -7 => Ok(Self::Internal),
            _ => Err(value),
        }
    }
}

/// Mode
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live = 0,
    Preview = 1,
}
impl TryFrom<i32> for Mode {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Live),
            1 => Ok(Self::Preview),
            _ => Err(value),
        }
    }
}

/// LogLevel
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}
impl TryFrom<i32> for LogLevel {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Trace),
            1 => Ok(Self::Debug),
            2 => Ok(Self::Info),
            3 => Ok(Self::Warn),
            4 => Ok(Self::Error),
            _ => Err(value),
        }
    }
}

/// 能力位，可按位或组合；host拒绝未知位。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability(i32);
#[allow(non_upper_case_globals)]
impl Capability {
    pub const None: Self = Self(0);
    pub const Preedit: Self = Self(1);
    pub const Resident: Self = Self(2);
    pub const fn bits(self) -> i32 {
        self.0
    }
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
    pub const fn from_bits(value: i32) -> Option<Self> {
        if value & !3 == 0 {
            Some(Self(value))
        } else {
            None
        }
    }
}
impl core::ops::BitOr for Capability {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl core::ops::BitOrAssign for Capability {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl TryFrom<i32> for Capability {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        Self::from_bits(value).ok_or(value)
    }
}

/// 装饰层可动画属性；偏移为DIP，缩放以层左上角为原点。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerProperty {
    Opacity = 0,
    OffsetX = 1,
    OffsetY = 2,
    ScaleX = 3,
    ScaleY = 4,
}
impl TryFrom<i32> for LayerProperty {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Opacity),
            1 => Ok(Self::OffsetX),
            2 => Ok(Self::OffsetY),
            3 => Ok(Self::ScaleX),
            4 => Ok(Self::ScaleY),
            _ => Err(value),
        }
    }
}

/// SDK和原生动画的时间曲线。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear = 0,
    SmoothStep = 1,
    EaseIn = 2,
    EaseOut = 3,
}
impl TryFrom<i32> for Easing {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Linear),
            1 => Ok(Self::SmoothStep),
            2 => Ok(Self::EaseIn),
            3 => Ok(Self::EaseOut),
            _ => Err(value),
        }
    }
}

/// 停止在当前显示值或立即到达目标。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerStop {
    Current = 0,
    End = 1,
}
impl TryFrom<i32> for LayerStop {
    type Error = i32;
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            0 => Ok(Self::Current),
            1 => Ok(Self::End),
            _ => Err(value),
        }
    }
}
