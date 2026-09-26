//! 为文本服务登记或撤销 COM 服务器、TSF 语言配置文件及类别。
//!
//! 注册按 COM 服务器、语言配置文件、TSF 类别的顺序执行；中途失败时按相反方向回滚已完成
//! 的步骤。此模块只负责注册表和 TSF 管理器交互，不参与运行期输入处理。

use crate::{CLSID_WEASEL_TIP, bindings};
use bindings::{
    CLSCTX_INPROC_SERVER, CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GUID_TFCAT_CATEGORY_OF_TIP,
    GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    GUID_TFCAT_PROP_AUDIODATA, GUID_TFCAT_PROP_INKDATA, GUID_TFCAT_PROPSTYLE_CUSTOM,
    GUID_TFCAT_PROPSTYLE_STATIC, GUID_TFCAT_PROPSTYLE_STATICCOMPACT, GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_COMLESS, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIPCAP_WOW16,
    GUID_WEASEL_PROFILE, GetModuleFileNameW, GetModuleHandleExW, HKL, HMODULE, ITfCategoryMgr,
    ITfInputProcessorProfileMgr,
};
use windows_core::{Error, GUID, Result};

use windows_registry::CLASSES_ROOT;
use windows_strings::{HSTRING, PCWSTR, PWSTR};

const DESCRIPTION: &str = "小狼毫RS";
const CLSID_KEY_PREFIX: &str = "CLSID\\";
const INPROC_SERVER: &str = "InprocServer32";
const THREADING_MODEL: &str = "ThreadingModel";

const PROFILE_LANGUAGES: [u16; 5] = [
    0x0804, // Chinese (Simplified)
    0x0404, // Chinese (Traditional)
    0x0c04, // Chinese (Hong Kong)
    0x1404, // Chinese (Macao)
    0x1004, // Chinese (Singapore)
];

const CATEGORIES: [&GUID; 16] = [
    &GUID_TFCAT_CATEGORY_OF_TIP,
    &GUID_TFCAT_TIP_KEYBOARD,
    &GUID_TFCAT_TIPCAP_SECUREMODE,
    &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    &GUID_TFCAT_TIPCAP_COMLESS,
    &GUID_TFCAT_TIPCAP_WOW16,
    &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    &GUID_TFCAT_PROP_AUDIODATA,
    &GUID_TFCAT_PROP_INKDATA,
    &GUID_TFCAT_PROPSTYLE_CUSTOM,
    &GUID_TFCAT_PROPSTYLE_STATIC,
    &GUID_TFCAT_PROPSTYLE_STATICCOMPACT,
    &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    &GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY,
];

/// 将 GUID 转换为 Windows 注册表惯用的大写、带花括号字符串形式。
fn guid_string(guid: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

/// 生成本 TIP 的 COM 类注册表子键名。
fn clsid_key() -> String {
    format!("{CLSID_KEY_PREFIX}{}", guid_string(&CLSID_WEASEL_TIP))
}

/// 获取当前 DLL 的完整路径，供 COM InprocServer32 和 TSF 图标注册使用。
///
/// 以导出函数地址定位模块，逐步扩展 UTF-16 缓冲区；达到 32 Ki 单元上限或 Win32 调用失败
/// 时返回系统错误，调用方据此中止注册。
pub(crate) fn module_path() -> Result<HSTRING> {
    let mut module = HMODULE::default();
    let address = PCWSTR(crate::DllRegisterServer as *const () as *const u16);
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS as u32,
            address,
            &mut module,
        )
    };
    if !ok.as_bool() {
        return Err(Error::from_thread());
    }

    let mut buffer = vec![0u16; 260];
    loop {
        let length = unsafe {
            GetModuleFileNameW(
                Some(module),
                PWSTR::from_raw(buffer.as_mut_ptr()),
                buffer.len() as u32,
            )
        };
        if length == 0 {
            return Err(Error::from_thread());
        }
        if length < buffer.len() as u32 - 1 {
            return Ok(HSTRING::from_wide(&buffer[..length as usize]));
        }
        if buffer.len() >= 32 * 1024 {
            return Err(Error::from_thread());
        }
        buffer.resize(buffer.len() * 2, 0);
    }
}

/// 写入 COM 类和进程内服务器信息；线程模型明确登记为 Apartment。
fn register_server() -> Result<()> {
    let root = CLASSES_ROOT.create(clsid_key())?;
    root.set_string("", DESCRIPTION)?;
    let inproc = root.create(INPROC_SERVER)?;
    inproc.set_hstring("", &module_path()?)?;
    inproc.set_string(THREADING_MODEL, "Apartment")?;
    Ok(())
}

/// 向 TSF 注册五种中文语言配置文件。
///
/// 默认仅启用简体中文；`TEXTSERVICE_PROFILE` 可按配置名选择启用项。首个 TSF 错误即停止，
/// 并由上层 `register` 撤销此前写入的 COM 服务器项。
fn register_profiles() -> Result<()> {
    let manager: ITfInputProcessorProfileMgr = unsafe {
        bindings::CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?
    };
    let description = HSTRING::from(DESCRIPTION);
    let icon = module_path()?;
    let icon_index = crate::icons::PROFILE_ICON_INDEX;
    let enabled_profile = std::env::var("TEXTSERVICE_PROFILE").ok();

    for (index, language) in PROFILE_LANGUAGES.into_iter().enumerate() {
        let language_name = match index {
            0 => "hans",
            1 => "hant",
            2 => "hongkong",
            3 => "macau",
            _ => "singapore",
        };
        let enabled = enabled_profile.as_deref().map_or(index == 0, |profile| {
            profile.eq_ignore_ascii_case(language_name)
        });
        let hr = unsafe {
            manager.RegisterProfile(
                &CLSID_WEASEL_TIP,
                bindings::LANGID(language),
                &GUID_WEASEL_PROFILE,
                description.as_ptr(),
                description.len() as u32,
                icon.as_ptr(),
                icon.len() as u32,
                icon_index,
                HKL::default(),
                0,
                enabled,
                0,
            )
        };
        if hr.is_err() {
            return Err(Error::from_hresult(hr));
        }
    }
    Ok(())
}

/// 尽力撤销所有语言配置文件；管理器不可用或单项失败时继续清理。
fn unregister_profiles() {
    let Ok(manager) = (unsafe {
        bindings::CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        )
    }) else {
        return;
    };
    for language in PROFILE_LANGUAGES {
        unsafe {
            let _ = manager.UnregisterProfile(
                &CLSID_WEASEL_TIP,
                bindings::LANGID(language),
                &GUID_WEASEL_PROFILE,
                0,
            );
        }
    }
}

/// 将本 TIP 登记到所声明的 TSF 能力及属性类别。
///
/// 首个失败 HRESULT 转为 `Error` 返回；上层负责撤销此前注册的配置文件和 COM 类。
fn register_categories() -> Result<()> {
    let manager: ITfCategoryMgr =
        unsafe { bindings::CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };
    for category in CATEGORIES {
        let hr =
            unsafe { manager.RegisterCategory(&CLSID_WEASEL_TIP, category, &CLSID_WEASEL_TIP) };
        if hr.is_err() {
            return Err(Error::from_hresult(hr));
        }
    }
    Ok(())
}

/// 尽力撤销所有 TIP 类别，容忍管理器创建或单项撤销失败。
fn unregister_categories() {
    let Ok(manager) = (unsafe {
        bindings::CoCreateInstance::<_, ITfCategoryMgr>(
            &CLSID_TF_CategoryMgr,
            None,
            CLSCTX_INPROC_SERVER,
        )
    }) else {
        return;
    };
    for category in CATEGORIES {
        unsafe {
            let _ = manager.UnregisterCategory(&CLSID_WEASEL_TIP, category, &CLSID_WEASEL_TIP);
        }
    }
}

/// 删除本 TIP 的 COM 类注册树。
fn unregister_server() {
    let _ = CLASSES_ROOT.remove_tree(clsid_key());
}

/// 按依赖顺序完成 COM 与 TSF 注册，并在任一步失败时回滚已完成步骤。
pub(crate) fn register() -> Result<()> {
    register_server()?;
    if let Err(error) = register_profiles() {
        unregister_server();
        return Err(error);
    }
    if let Err(error) = register_categories() {
        unregister_profiles();
        unregister_server();
        return Err(error);
    }
    Ok(())
}

/// 按类别、配置文件、COM 服务器的逆序尽力撤销注册。
pub(crate) fn unregister() {
    unregister_categories();
    unregister_profiles();
    unregister_server();
}
