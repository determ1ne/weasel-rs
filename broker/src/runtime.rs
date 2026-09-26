//! Broker 常驻模式的顶层启动与关闭编排。

use std::thread;

use weasel_common::{
    logging::ComponentLogger,
    process::{RuntimePaths, SingleInstance},
    rpc::default_renderer_pipe_name,
};

use crate::{operations, service_supervisor, tray};

/// 判断更新器是否可以请求 broker 退出。
pub(crate) fn can_shutdown_for_update() -> bool {
    !operations::is_busy() && !service_supervisor::is_stopping()
}

/// 初始化并运行常驻 broker。
///
/// 初始化顺序保证设置 RPC 先于 renderer/server 可用；关闭时则先停止监控和后台操作，
/// 再依次请求受管服务退出，避免监控线程在清理过程中重新拉起组件。
pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _instance = match SingleInstance::acquire("broker") {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("weasel-broker: {error}");
            return Ok(());
        }
    };

    let paths = RuntimePaths::discover()?;
    paths.ensure()?;
    let logger = ComponentLogger::for_paths(&paths, "broker")?;
    service_supervisor::initialize(logger.clone())?;

    let mut settings_warnings = Vec::new();
    let settings = crate::settings::load(&paths, |warning| {
        service_supervisor::diagnostic(&warning);
        settings_warnings.push(warning);
    });
    // 预览功能会通过同一服务重新读取磁盘配置，因此该守卫必须存活到消息循环结束。
    let settings_service = crate::settings_rpc::SettingsService::start(settings, paths.clone())?;
    settings_service
        .notifications()
        .report_settings_errors(&settings_warnings);

    let directory = paths.executable_directory.clone();
    crate::child_process::initialize()?;
    crate::child_process::clear_stale(&directory, "weasel-server.exe")?;
    crate::child_process::clear_stale(&directory, "weasel-renderer.exe")?;

    // renderer 先启动，确保 server 发布第一份候选快照时已有接收端。
    let renderer = service_supervisor::start_child(&directory, "weasel-renderer.exe", &[])?;
    let server = match service_supervisor::start_child(&directory, "weasel-server.exe", &[]) {
        Ok(server) => server,
        Err(error) => {
            let mut renderer = Some(renderer);
            if let Err(cleanup) = service_supervisor::shutdown_component(
                &mut renderer,
                default_renderer_pipe_name(),
                "broker startup failed",
            ) {
                service_supervisor::diagnostic(&format!("Startup rollback: {cleanup}"));
            }
            return Err(error.into());
        }
    };
    service_supervisor::install(
        settings_service.settings(),
        settings_service.notifications(),
        paths,
        server,
        renderer,
    )?;

    let monitor = thread::Builder::new()
        .name("weasel-service-monitor".into())
        .spawn(service_supervisor::monitor_children)?;

    let tray = match tray::create() {
        Ok(tray) => tray,
        Err(error) => {
            service_supervisor::request_stop();
            let _ = monitor.join();
            service_supervisor::shutdown_all();
            return Err(error);
        }
    };
    tray::start_updater(&directory, &tray, &logger);
    tray::message_loop();
    tray::stop_updater();

    service_supervisor::request_stop();
    let _ = monitor.join();
    // 操作线程观察到停止状态后会归还其临时持有的子进程句柄。
    operations::join();
    service_supervisor::shutdown_all();
    drop(tray);
    drop(settings_service);
    Ok(())
}
