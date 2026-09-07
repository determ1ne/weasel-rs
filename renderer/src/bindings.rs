#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

pub use Windows::Foundation::Size;
pub use Windows::UI::Color;
pub use Windows::UI::ViewManagement::{UIColorType, UISettings};
pub use Windows::UI::Xaml::Controls::{
    Border, ColumnDefinition, Grid, Orientation, StackPanel, TextBlock,
};
pub use Windows::UI::Xaml::Hosting::{DesktopWindowXamlSource, WindowsXamlManager};
pub use Windows::UI::Xaml::Media::{
    AcrylicBackgroundSource, AcrylicBrush, FontFamily, SolidColorBrush,
};
pub use Windows::UI::Xaml::{
    CornerRadius, ElementTheme, GridLength, GridUnitType, HorizontalAlignment, Thickness,
    VerticalAlignment,
};
pub use Windows::Win32::{
    CreateWindowExW, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DefWindowProcW, DestroyWindow, DwmSetWindowAttribute, GWL_EXSTYLE, GetDpiForWindow,
    GetModuleHandleW, GetMonitorInfoW, GetPropW, GetWindowLongPtrW, HANDLE, HICON, HWND,
    HWND_TOPMOST, ICON_BIG, ICON_SMALL, IDesktopWindowXamlSourceNative, LPARAM, LRESULT, LoadIconW,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, PostQuitMessage, PostThreadMessageW,
    RECT, RegisterClassW, RemovePropW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SendMessageW, SetPropW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_CLOSE,
    WM_DPICHANGED, WM_SETICON, WM_SIZE, WNDCLASSW, WPARAM, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
