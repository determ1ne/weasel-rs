//! 仅供安装流程调用的 Windows 快捷方式创建功能。
//!
//! Windows Toast 通知需要系统上存在具有 AppUserModel ID 的快捷方式
use std::path::Path;

use windows_core::Interface;
use windows_strings::{HSTRING, w};

use crate::bindings::*;
use weasel_common::comrt::ComApartment;

/// 持有一个由本模块分配字符串的 PROPVARIANT，并在销毁时释放其内容。
struct StringProperty(PROPVARIANT);

impl StringProperty {
    /// 为文本分配独立的宽字符串，并将其包装为可由 `PropVariantClear` 释放的属性值。
    ///
    /// 分配失败时传播 Windows 错误；成功返回的包装对象独占该字符串的清理责任。
    fn new(text: &str) -> windows_core::Result<Self> {
        let mut value = PROPVARIANT::default();
        unsafe {
            let string = SHStrDupW(&HSTRING::from(text))?;
            value.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VARTYPE(VT_LPWSTR as u16),
                Anonymous: PROPVARIANT_0_0_0 { pwszVal: string },
                ..Default::default()
            });
        }
        Ok(Self(value))
    }
}

impl Drop for StringProperty {
    /// 清理 PROPVARIANT 持有的字符串；清理错误无法在析构期间返回。
    fn drop(&mut self) {
        unsafe {
            let _ = PropVariantClear(&mut self.0);
        }
    }
}

/// 将当前 broker 注册为指定绝对路径上的快捷方式。
///
/// 快捷方式包含 broker 可执行文件、空参数、工作目录、描述、图标及 Toast AppUserModel ID。
/// COM 初始化和所有接口操作均留在当前 STA 线程；路径非法或任一 Windows/文件操作失败时返回错误。
pub(crate) fn install(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err("shortcut path must be absolute".into());
    }
    let executable = std::env::current_exe()?;
    let directory = executable.parent().ok_or("broker directory unavailable")?;
    // Declared before the interfaces so COM is uninitialized last, including errors.
    let _apartment = ComApartment::initialize_sta()?;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER as _)?;
        link.SetPath(&HSTRING::from(executable.as_os_str())).ok()?;
        link.SetArguments(w!("")).ok()?;
        link.SetWorkingDirectory(&HSTRING::from(directory.as_os_str()))
            .ok()?;
        link.SetDescription(w!("小狼毫RS算法服务")).ok()?;
        link.SetIconLocation(&HSTRING::from(executable.as_os_str()), 0)
            .ok()?;
        let properties: IPropertyStore = link.cast()?;
        let app_id = StringProperty::new(crate::toast::APP_ID)?;
        properties.SetValue(&PKEY_AppUserModel_ID, &app_id.0).ok()?;
        properties.Commit().ok()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(path.as_os_str()), true).ok()?;
    }
    Ok(())
}
