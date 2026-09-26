//! 每用户运行的代理进程，负责托盘入口以及算法服务和渲染器子进程的生命周期。
//!
//! 安装器调用的维护命令在初始化托盘和托管子进程之前处理，避免短命任务意外启动常驻服务。
#![windows_subsystem = "windows"]

mod child_process;
mod installer;
mod lifecycle;
mod notifications;
mod operations;
mod runtime;
mod service_rpc;
mod service_supervisor;
mod settings;
mod settings_rpc;
mod shortcut;
mod shutdown;
mod toast;
mod updater;

mod bindings;

mod menu_actions;
mod tray;

/// 分发安装维护命令，或启动常驻托盘代理。
///
/// 维护命令只接受各自约定的参数；额外参数和未知选项会作为错误返回，供安装器发现调用错误。
/// 托盘运行失败时同时写入标准错误并显示 Windows 错误对话框，再将原错误交还给进程入口。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();

    // 退出算法服务
    if command.as_deref() == Some(std::ffi::OsStr::new("--shutdown")) {
        let directory = args.next().map(std::path::PathBuf::from).unwrap_or(
            std::env::current_exe()?
                .parent()
                .ok_or("missing executable directory")?
                .to_owned(),
        );
        if args.next().is_some() {
            return Err("unexpected shutdown arguments".into());
        }
        return shutdown::run(&directory);
    }

    // 安装快捷方式
    if command.as_deref() == Some(std::ffi::OsStr::new("--install-shortcut")) {
        let path = args.next().ok_or("missing shortcut path")?;
        if args.next().is_some() {
            return Err("unexpected shortcut arguments".into());
        }
        return shortcut::install(std::path::Path::new(&path));
    }

    // 安装后部署
    if command.as_deref() == Some(std::ffi::OsStr::new("--post-install")) {
        if args.next().is_some() {
            return Err("unexpected post-install arguments".into());
        }
        return installer::post_install();
    }

    // 卸载后清理
    if command.as_deref() == Some(std::ffi::OsStr::new("--post-uninstall")) {
        if args.next().is_some() {
            return Err("unexpected post-uninstall arguments".into());
        }
        return installer::post_uninstall();
    }

    // 未知命令处理
    if command.is_some() {
        return Err("unknown broker option".into());
    }

    // 启动常驻托盘代理
    if let Err(error) = runtime::run() {
        eprintln!("weasel-broker: {error}");
        unsafe {
            let _ = bindings::MessageBoxW(
                None,
                &windows_strings::HSTRING::from(format!("算法服务启动或运行失败：\n{error}")),
                &windows_strings::HSTRING::from("小狼毫RS"),
                (bindings::MB_OK | bindings::MB_ICONERROR | bindings::MB_SETFOREGROUND) as u32,
            );
        }
        return Err(error);
    }
    Ok(())
}
