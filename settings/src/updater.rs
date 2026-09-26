//! 只访问 WinSparkle 的用户偏好；设置应用不初始化更新检查器。
use libloading::Library;
use std::path::Path;

type SetAppDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
type GetAutomaticChecks = unsafe extern "C" fn() -> i32;
type SetAutomaticChecks = unsafe extern "C" fn(i32);

/// 在 DLL 生命周期内载入 WinSparkle，并以应用标识访问共享用户偏好。
///
/// 动态库路径固定为安装目录下的绝对路径。库及符号在闭包执行期间保持存活；
/// 任何载入或符号解析错误都会以字符串返回，且本模块不初始化更新检查器。
fn with_preferences<T>(
    directory: &Path,
    action: impl FnOnce(GetAutomaticChecks, SetAutomaticChecks) -> T,
) -> Result<T, String> {
    // 使用绝对的安装目录路径，避免从工作目录载入同名 DLL。
    let library = unsafe { Library::new(directory.join("WinSparkle.dll")) }
        .map_err(|error| format!("无法载入 WinSparkle.dll：{error}"))?;
    unsafe {
        let details: SetAppDetails = *library
            .get(b"win_sparkle_set_app_details\0")
            .map_err(|error| error.to_string())?;
        let get: GetAutomaticChecks = *library
            .get(b"win_sparkle_get_automatic_check_for_updates\0")
            .map_err(|error| error.to_string())?;
        let set: SetAutomaticChecks = *library
            .get(b"win_sparkle_set_automatic_check_for_updates\0")
            .map_err(|error| error.to_string())?;
        let wide = |value: &str| value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        // 必须与 broker/src/updater.rs 的应用标识一致，才能读写同一份偏好。
        details(
            wide("Weasel-RS").as_ptr(),
            wide("小狼毫RS").as_ptr(),
            wide(env!("CARGO_PKG_VERSION")).as_ptr(),
        );
        Ok(action(get, set))
    }
}

/// 读取安装目录对应的 WinSparkle 自动检查更新偏好。
pub fn read(directory: &Path) -> Result<bool, String> {
    with_preferences(directory, |get, _| unsafe { get() != 0 })
}

/// 写入自动检查更新偏好，并立即回读核验写入结果。
///
/// DLL 或导出函数不可用、回读值与目标不符时返回错误；该操作只改偏好，不启动检查器。
pub fn write(directory: &Path, enabled: bool) -> Result<(), String> {
    with_preferences(directory, |get, set| unsafe {
        set(i32::from(enabled));
        if (get() != 0) == enabled {
            Ok(())
        } else {
            Err("WinSparkle 未能保存自动检查更新设置".to_owned())
        }
    })?
}
