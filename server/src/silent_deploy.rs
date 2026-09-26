//! 在部署子进程初始化 Rust 与原生 DLL CRT 之前重定向标准流。
//!
//! 仅修改 tracing 配置或在 CRT 初始化后调用 `SetStdHandle`，都不能可靠屏蔽 librime
//! 的输出。此包装进程以空标准流启动普通部署子进程；文件日志和数据目录锁仍由子进程
//! 按常规部署流程管理。
use std::{
    ffi::OsString,
    os::windows::process::CommandExt,
    process::{Command, ExitCode, Stdio},
};

/// 判断参数是否恰好表示静默普通部署。
///
/// 两个参数可以任意顺序，但必须各自为 `--deploy` 与 `--silent`；其他选项或重复参数
/// 均不属于此包装器处理的命令形式。
pub fn valid_arguments(args: &[OsString]) -> bool {
    args.len() == 2
        && args.iter().any(|arg| arg == "--deploy")
        && args.iter().any(|arg| arg == "--silent")
}

/// 将子进程的标准输入、输出和错误流全部重定向到空设备后等待退出。
///
/// 禁用控制台窗口；返回启动或等待子进程时的 I/O 错误。子进程退出码保持原样供调用方
/// 判定部署是否成功。
fn status(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(crate::bindings::CREATE_NO_WINDOW as u32)
        .status()
}

/// 以静默模式重新启动当前可执行文件的普通部署入口。
///
/// `--silent` 只用于选择当前包装器，不会传递给子进程；子进程失败或无法启动时返回
/// 失败退出码，只有部署子进程成功时返回成功码。
pub fn run() -> ExitCode {
    let result = std::env::current_exe().and_then(|exe| {
        // Do not forward --silent: only this parent is the redirection wrapper.
        status(Command::new(exe).arg("--deploy"))
    });
    match result {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_is_only_valid_for_plain_deployment() {
        let valid =
            |args: &[&str]| valid_arguments(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(valid(&["--deploy", "--silent"]));
        assert!(valid(&["--silent", "--deploy"]));
        assert!(!valid(&["--silent"]));
        assert!(!valid(&["--deploy-ui", "--silent"]));
        assert!(!valid(&["--deploy", "--silent", "--deploy-ui"]));
    }

    #[test]
    fn redirected_child_preserves_failure_status() {
        // A shell fixture only: no Rime loading, deployment or service startup.
        let shell = std::env::var_os("COMSPEC").expect("Windows command processor");
        let result = status(Command::new(shell).args([
            "/D",
            "/C",
            "echo stdout & echo stderr 1>&2 & exit /b 7",
        ]))
        .expect("run redirected fixture");
        assert_eq!(result.code(), Some(7));
    }
}
