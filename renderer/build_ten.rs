/// Called by the renderer's shared build.rs.
pub fn generate() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    windows_bindgen::builder()
        .input_default()
        .output(out.join("ten-bindings.rs"))
        .filters([
            "Windows.Win32.D2D1CreateFactory", "Windows.Win32.DWriteCreateFactory",
            "Windows.Win32.ID2D1Factory::CreateHwndRenderTarget", "Windows.Win32.ID2D1HwndRenderTarget::Resize",
            "Windows.Win32.ID2D1SolidColorBrush::SetColor", "Windows.Win32.IDWriteFactory::{CreateTextFormat,CreateTextLayout}",
            "Windows.Win32.IDWriteTextFormat::{SetTextAlignment,SetParagraphAlignment,SetWordWrapping}",
            "Windows.Win32.IDWriteTextLayout::GetMetrics",
            "Windows.Win32.ID2D1RenderTarget::{SetDpi,CreateSolidColorBrush,BeginDraw,EndDraw,Clear,FillRectangle,DrawText}",
            "Windows.Win32.D2D1_FACTORY_TYPE", "Windows.Win32.D2D1_PRESENT_OPTIONS",
            "Windows.Win32.D2D1_DRAW_TEXT_OPTIONS", "Windows.Win32.D2D1_ALPHA_MODE",
            "Windows.Win32.D2DERR_RECREATE_TARGET",
            "Windows.Win32.E_UNEXPECTED",
            "Windows.Win32.DWRITE_FACTORY_TYPE", "Windows.Win32.DWRITE_FONT_WEIGHT",
            "Windows.Win32.DWRITE_FONT_STYLE", "Windows.Win32.DWRITE_FONT_STRETCH",
            "Windows.Win32.DWRITE_TEXT_ALIGNMENT", "Windows.Win32.DWRITE_PARAGRAPH_ALIGNMENT",
            "Windows.Win32.DWRITE_WORD_WRAPPING", "Windows.Win32.DWRITE_MEASURING_MODE",
            "Windows.Win32.BeginPaint", "Windows.Win32.EndPaint", "Windows.Win32.InvalidateRect",
            "Windows.Win32.GetModuleHandleW", "Windows.Win32.RegisterClassW",
            "Windows.Win32.GetClassInfoW", "Windows.Win32.LoadCursorW", "Windows.Win32.IDC_ARROW",
            "Windows.Win32.CreateWindowExW", "Windows.Win32.DestroyWindow",
            "Windows.Win32.DefWindowProcW", "Windows.Win32.GetWindowLongPtrW",
            "Windows.Win32.SetWindowLongPtrW", "Windows.Win32.GWLP_USERDATA",
            "Windows.Win32.GetClientRect", "Windows.Win32.SetWindowPos", "Windows.Win32.ShowWindow",
            "Windows.Win32.GetDpiForWindow", "Windows.Win32.SetCapture", "Windows.Win32.GetCapture",
            "Windows.Win32.ReleaseCapture", "Windows.Win32.TrackMouseEvent", "Windows.Win32.TME_LEAVE",
            "Windows.Win32.SetTimer", "Windows.Win32.KillTimer",
            "Windows.Win32.WS_POPUP", "Windows.Win32.WS_EX_TOPMOST", "Windows.Win32.WS_EX_TOOLWINDOW",
            "Windows.Win32.WS_EX_NOACTIVATE", "Windows.Win32.HWND_TOPMOST", "Windows.Win32.SWP_NOACTIVATE",
            "Windows.Win32.SW_SHOWNOACTIVATE", "Windows.Win32.SW_HIDE",
            "Windows.Win32.WM_NCCREATE", "Windows.Win32.WM_NCDESTROY", "Windows.Win32.WM_PAINT",
            "Windows.Win32.WM_ERASEBKGND", "Windows.Win32.WM_SIZE", "Windows.Win32.WM_DPICHANGED",
            "Windows.Win32.WM_LBUTTONDOWN", "Windows.Win32.WM_LBUTTONUP", "Windows.Win32.WM_MOUSEMOVE",
            "Windows.Win32.WM_MOUSELEAVE", "Windows.Win32.WM_CAPTURECHANGED", "Windows.Win32.WM_CANCELMODE",
            "Windows.Win32.WM_MOUSEACTIVATE", "Windows.Win32.MA_NOACTIVATE", "Windows.Win32.WM_TIMER",
            "Windows.Win32.CREATESTRUCTW",
        ])
        .write();
    println!("cargo:rerun-if-changed=build_ten.rs");
}
