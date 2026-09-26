//! 安装后的部署和当前用户自启动项维护在交互用户上下文中执行，避免提升权限的
//! 安装进程直接改写用户服务状态。

use std::{path::PathBuf, process::Command};

use windows_registry::CURRENT_USER;

/// 当前用户登录时运行 broker 的注册表键。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// broker 自启动命令使用的注册表值名称。
const RUN_VALUE: &str = "小狼毫RS算法服务";

/// 获取当前 broker 可执行文件所在目录。
///
/// 若当前可执行文件没有父目录则返回错误；该目录也是安装后服务命令的工作目录。
fn executable_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::current_exe()?
        .parent()
        .ok_or("broker executable has no parent directory")?
        .to_owned())
}

/// 构造带引号的 broker 自启动命令，确保含空格的安装路径作为单一路径解析。
fn startup_command(directory: &std::path::Path) -> String {
    format!("\"{}\"", directory.join("weasel-broker.exe").display())
}

/// 执行安装后部署，并在当前用户 Run 项尚未设置时登记 broker 自启动。
///
/// 部署失败会中止并返回错误。若已有同名注册表值则保留原值，不覆盖用户配置；
/// 注册表访问或写入失败同样会返回错误。
pub(crate) fn post_install() -> Result<(), Box<dyn std::error::Error>> {
    let directory = executable_directory()?;
    let status = Command::new(directory.join("weasel-server.exe"))
        .args(["--deploy", "--silent"])
        .current_dir(&directory)
        .status()?;
    if !status.success() {
        return Err(format!("Rime deployment failed with {status}").into());
    }

    let run = CURRENT_USER.create(RUN_KEY)?;
    let expected = startup_command(&directory);
    // 如果自启动项已经存在就保留，避免覆盖用户手动修改的启动命令
    if run.get_string(RUN_VALUE).is_err() {
        run.set_string(RUN_VALUE, expected)?;
    }
    Ok(())
}

/// 移除指向当前安装目录的自启动项。
///
/// 注册表键不存在时视为已清理；若同名值已被用户改成其他命令则保留该值。
pub(crate) fn post_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let directory = executable_directory()?;
    let Ok(run) = CURRENT_USER.open(RUN_KEY) else {
        return Ok(());
    };
    let expected = startup_command(&directory);
    if run.get_string(RUN_VALUE).ok().as_deref() == Some(expected.as_str()) {
        run.remove_value(RUN_VALUE)?;
    }
    Ok(())
}
