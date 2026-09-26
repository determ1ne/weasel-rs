// Generated from themes/wasm/abi.json. DO NOT EDIT.
export const ABI_VERSION:i32 = 2;
// 只读快照整数属性。ItemEnabled使用候选index，其余index必须为0。AsciiMode/TotalItemCount的-1表示未知。
export enum ViewField {
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
  HasModeIndicator = 19,
  ModeIndicatorId = 20,
  ModeIndicatorAscii = 21,
  ModeIndicatorReason = 22,
}

// 中英文模式提示来源；仅在HasModeIndicator非零时读取。
export enum ModeIndicatorReason {
  Focus = 1,
  UserSwitch = 2,
}

// 只读快照字符串属性。Primary/Secondary使用候选index，Preedit的index必须为0。
export enum ViewStringField {
  Primary = 0,
  Secondary = 1,
  Preedit = 2,
}

// 只暴露模块配置与允许读取的全局展示设置，不包含其他应用配置。
export enum ConfigScope {
  Module = 1,
  Global = 2,
}

// DataKind
export enum DataKind {
  Missing = 0,
  Null = 1,
  Bool = 2,
  Number = 3,
  String = 4,
  Array = 5,
  Object = 6,
}

// 资源度量，单位DIP；Baseline仅适用于文本布局。
export enum ResourceMetric {
  Width = 0,
  Height = 1,
  Baseline = 2,
}

// host到guest的统一事件种类。
export enum EventKind {
  View = 0,
  Appearance = 1,
  Hide = 2,
  Pointer = 3,
  Animation = 4,
}

// 取消和离开不能提交点击；取消表示捕获丢失或宿主取消。
export enum PointerPhase {
  Down = 0,
  Move = 1,
  Up = 2,
  Leave = 3,
  Cancel = 4,
}

// Action
export enum Action {
  Item = 0,
  Previous = 1,
  Next = 2,
  Emoji = 3,
  Dismiss = 4,
}

// 事件成功返回值。Keep丢弃本次画面修改，Present替换整帧（可为空）；失败返回负ErrorCode。
export enum FrameResult {
  Keep = 0,
  Present = 1,
}

// 创建/资源操作：0成功或正句柄，负数失败。事件只允许FrameResult或负错误码。
export enum ErrorCode {
  Success = 0,
  NotFound = -1,
  InvalidArgument = -2,
  InvalidHandle = -3,
  ResourceLimit = -6,
  Internal = -7,
}

// Mode
export enum Mode {
  Live = 0,
  Preview = 1,
}

// 由宿主管理的原生表面用途提示。Primary 的固定 ID 为 0，不能通过 surface_create 创建或销毁；其余类型不暴露 HWND，当前作为语义标记保留给后续宿主策略。
export enum SurfaceKind {
  Primary = 0,
  Transient = 1,
  Resident = 2,
  Auxiliary = 3,
}

// LogLevel
export enum LogLevel {
  Trace = 0,
  Debug = 1,
  Info = 2,
  Warn = 3,
  Error = 4,
}

// 能力位，可按位或组合；host拒绝未知位。
export enum Capability {
  None = 0,
  Preedit = 1,
  Resident = 2,
  ModeIndicator = 4,
}

// 装饰层可动画属性；偏移为DIP，缩放以层左上角为原点。
export enum LayerProperty {
  Opacity = 0,
  OffsetX = 1,
  OffsetY = 2,
  ScaleX = 3,
  ScaleY = 4,
}

// SDK和原生动画的时间曲线。
export enum Easing {
  Linear = 0,
  SmoothStep = 1,
  EaseIn = 2,
  EaseOut = 3,
}

// 停止在当前显示值或立即到达目标。
export enum LayerStop {
  Current = 0,
  End = 1,
}
