//! 负责以受限 DLL 搜索路径加载 librime，并取得其版本化 API 表入口。
//!
//! `RimeLibrary` 持有模块句柄；调用方须让该对象覆盖所有从 DLL 取得的函数指针
//! 使用期，之后释放时才卸载模块。

use std::{path::Path, ptr::NonNull};

use super::raw;

use crate::bindings::{
    FreeLibrary, GetProcAddress, HMODULE, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_strings::{HSTRING, PCWSTR, s};

/// 已加载的 librime DLL 及其 API 表指针。
///
/// API 指针仅在 DLL 保持加载期间有效；通常由引擎持有本对象，并由会话延长引擎
/// 生命周期。此类型不自行解释表布局，版本和字段边界由 `api` 模块验证。
pub struct RimeLibrary {
    /// Windows 模块句柄，析构时释放。
    module: HMODULE,
    /// 从 DLL 的 `rime_get_api` 导出取得的非空表地址。
    api: NonNull<raw::RimeApi>,
}

impl RimeLibrary {
    /// 加载指定 DLL 并解析 `rime_get_api`。
    ///
    /// 先规范化路径，再限制依赖项搜索到 DLL 所在目录和 System32。路径解析、模块
    /// 加载、导出缺失或导出返回空指针均以错误返回；失败路径会释放已加载模块。
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(path)
            .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
        let wide = HSTRING::from(path.as_path());
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR::from_raw(wide.as_ptr()),
                Default::default(),
                (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) as u32,
            )
        };
        // Generated bindings return the raw handle, not a Result.
        if module.0.is_null() {
            return Err(format!(
                "LoadLibraryExW({}) failed: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }

        let symbol = unsafe { GetProcAddress(module, s!("rime_get_api")) };
        let Some(symbol) = symbol else {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(format!("rime_get_api was not found in {}", path.display()));
        };

        let get_api: unsafe extern "C" fn() -> *mut raw::RimeApi =
            unsafe { std::mem::transmute(symbol) };
        let api = unsafe { get_api() };
        let Some(api) = NonNull::new(api) else {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err("rime_get_api returned a null API pointer".to_owned());
        };

        Ok(Self { module, api })
    }

    /// 返回 DLL 提供的 API 表地址。
    ///
    /// 指针的有效期不超过此 `RimeLibrary` 的生命周期；读取其字段前仍须按 ABI
    /// 声明的 `data_size` 检查边界。
    pub fn api(&self) -> NonNull<raw::RimeApi> {
        self.api
    }
}

impl Drop for RimeLibrary {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}
