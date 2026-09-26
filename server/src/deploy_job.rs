//! 在独立任务中运行 Rime 数据部署，并向界面和遥测输出有限日志预览。
//!
//! 部署进程的标准输出与错误输出会并发读取，避免子进程因管道写满而停滞。界面邮箱
//! 只保留有上限的预览文本，完成状态单独保存，不会被日志溢出挤掉。
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::io::AsyncReadExt;
use weasel_common::{deploy_protocol::DeployComplete, process::SingleInstance};

pub const PREVIEW_LIMIT: usize = 256 * 1024;

/// 部署工作线程向任意界面发布日志和最终状态的最小接口。
///
/// 实现不得在内部持有部署数据锁，也不应因界面暂时无响应而无限期阻塞。XAML 窗口使用
/// 有界邮箱实现；兼容 Task Dialog 使用其线程安全控制器实现。
pub trait UiSink: Send + Sync {
    /// 告知界面本次部署原始输出的落盘位置。
    fn log_path(&self, _path: &Path) {}
    /// 追加一段已经完成 UTF-8 解码的部署输出。
    fn log(&self, text: &str);
    /// 发布唯一的部署完成结果。
    fn finish(&self, done: DeployComplete);
}

/// 本次部署专用的原始输出文件。
///
/// 每次部署覆盖上一份 `deploy-output.log`，避免高频部署不断积累文件。每次写入都会刷新，
/// 因而用户可以在部署尚未结束时通过界面链接查看当前内容。
struct OutputLog {
    path: PathBuf,
    writer: BufWriter<std::fs::File>,
}

impl OutputLog {
    fn create() -> Result<Self, String> {
        let paths = weasel_common::process::RuntimePaths::discover().map_err(|e| e.to_string())?;
        paths.ensure().map_err(|e| e.to_string())?;
        let path = paths.logs.join("deploy-output.log");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("无法创建部署日志 {}：{e}", path.display()))?;
        let mut log = Self {
            path,
            writer: BufWriter::new(file),
        };
        log.record("INFO", "小狼毫RS Rime 数据部署开始。")
            .map_err(|e| e.to_string())?;
        Ok(log)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_output(&mut self, source: &str, text: &str) -> std::io::Result<()> {
        writeln!(
            self.writer,
            "{} {source}",
            weasel_common::logging::timestamp(std::time::SystemTime::now())
        )?;
        self.writer.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush()
    }

    fn record(&mut self, level: &str, text: &str) -> std::io::Result<()> {
        writeln!(
            self.writer,
            "{} {level} {text}",
            weasel_common::logging::timestamp(std::time::SystemTime::now())
        )?;
        self.writer.flush()
    }
}

/// 一次从界面邮箱取出的日志快照及部署结果。
///
/// `bytes` 仅统计保留日志的 UTF-8 字节数；`omitted` 表示此前至少有部分日志因单条过长
/// 或总量上限而未保留。
#[derive(Default)]
pub struct Preview {
    /// 本次部署专用日志的绝对路径；只在路径首次建立后交付一次。
    pub log_path: Option<PathBuf>,
    /// 按到达顺序保存的日志片段，总字节数不超过 `PREVIEW_LIMIT`。
    pub chunks: VecDeque<String>,
    /// 当前片段的 UTF-8 字节总数。
    bytes: usize,
    /// 是否因预览容量限制丢弃过文本。
    pub omitted: bool,
    /// 部署结束状态；独立于日志保留，始终可随下一次快照交付。
    pub complete: Option<DeployComplete>,
}

/// 以有界内存向 UI 合并部署日志与完成通知。
///
/// 写入方通过互斥锁更新快照，并用窗口消息提示 UI；通知合并期间不会重复投递消息。
#[derive(Default)]
pub struct UiMailbox {
    pending: Mutex<Preview>,
    window: AtomicUsize,
    wake_pending: AtomicBool,
}

impl UiMailbox {
    /// 设置接收部署更新消息的窗口句柄；零值表示暂时没有窗口可通知。
    pub fn set_window(&self, hwnd: usize) {
        self.window.store(hwnd, Ordering::Release);
    }
    /// 保存本次部署日志路径并提示 UI 显示打开入口。
    pub fn log_path(&self, path: &Path) {
        self.pending.lock().unwrap().log_path = Some(path.to_owned());
        self.notify();
    }
    /// 将日志追加到预览，必要时裁剪旧片段或当前文本，并尝试唤醒 UI。
    ///
    /// 单个超长片段只保留末尾至多 `PREVIEW_LIMIT` 字节，并从 UTF-8 字符边界开始；
    /// 因此 UI 的日志存储有明确上限，调用方也不会等待 UI 消费。
    pub fn log(&self, text: &str) {
        let mut state = self.pending.lock().unwrap();
        let mut start = text.len().saturating_sub(PREVIEW_LIMIT);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        state.omitted |= start > 0;
        state.chunks.push_back(text[start..].to_owned());
        state.bytes += text.len() - start;
        while state.bytes > PREVIEW_LIMIT || state.chunks.len() > 1000 {
            state.bytes -= state.chunks.pop_front().unwrap().len();
            state.omitted = true;
        }
        drop(state);
        self.notify();
    }
    /// 保存最终结果并提示 UI；该结果不受日志裁剪影响。
    pub fn finish(&self, done: DeployComplete) {
        self.pending.lock().unwrap().complete = Some(done);
        self.notify();
    }
    /// 合并窗口唤醒消息；投递失败时清除待唤醒标志，供后续更新重试。
    fn notify(&self) {
        let hwnd = self.window.load(Ordering::Acquire);
        if hwnd != 0 && !self.wake_pending.swap(true, Ordering::AcqRel) {
            use crate::ui_bindings::Windows::Win32::*;
            let posted = unsafe {
                PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    crate::deploy_ui::WM_DEPLOY_FINISHED,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            if !posted.as_bool() {
                self.wake_pending.store(false, Ordering::Release);
                diagnostic("could not post deployment update; next UI message will consume it");
            }
        }
    }
    /// 取走当前快照并允许下一轮窗口唤醒。
    ///
    /// 日志路径、日志、丢弃标志和完成状态会一并重置；调用方应在 UI 线程消费返回值。
    pub fn take(&self) -> Preview {
        let mut state = self.pending.lock().unwrap();
        self.wake_pending.store(false, Ordering::Release);
        std::mem::take(&mut *state)
    }
}

impl UiSink for UiMailbox {
    fn log_path(&self, path: &Path) {
        UiMailbox::log_path(self, path);
    }

    fn log(&self, text: &str) {
        UiMailbox::log(self, text);
    }

    fn finish(&self, done: DeployComplete) {
        UiMailbox::finish(self, done);
    }
}

/// 记录部署流程的诊断信息。
pub fn diagnostic(message: &str) {
    tracing::info!(message, "deployment diagnostic");
}

/// 执行部署子进程，并将有限日志预览、遥测和最终状态发送给接收方。
///
/// 此函数拥有单实例守卫直到部署结果确定。提供 UI 接收器时，由 UI 生命周期负责接收
/// 完成状态；无界面模式则最多等待两秒刷出标准输出遥测，避免继承的管道无限阻塞退出。
pub fn run(ui: Option<Arc<dyn UiSink>>, guard: SingleInstance) -> DeployComplete {
    diagnostic("deployment worker started");
    let telemetry = crate::deploy_telemetry::Telemetry::stdout();
    let mut output_log = match OutputLog::create() {
        Ok(log) => {
            if let Some(ui) = &ui {
                ui.log_path(log.path());
            }
            Some(log)
        }
        Err(error) => {
            diagnostic(&format!("deployment output log unavailable: {error}"));
            None
        }
    };
    let mut publish = |source: &str, text: String| {
        if let Some(log) = output_log.as_mut()
            && let Err(error) = log.write_output(source, &text)
        {
            diagnostic(&format!("deployment output log write failed: {error}"));
            output_log = None;
        }
        if let Some(ui) = &ui {
            ui.log(&text); // Never waits for the UI to consume a queue slot.
        }
        telemetry.log(source, text);
    };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
        .and_then(|runtime| runtime.block_on(run_process(&mut publish)));
    let done = match result {
        Ok(status) => DeployComplete {
            success: status.success(),
            exit_code: status.code(),
            message: if status.success() {
                "Rime 数据部署成功。".into()
            } else {
                format!("Rime 数据部署失败：{status}")
            },
        },
        Err(error) => DeployComplete {
            success: false,
            exit_code: None,
            message: error,
        },
    };
    drop(publish);
    if let Some(log) = output_log.as_mut() {
        let level = if done.success { "INFO" } else { "ERROR" };
        if let Err(error) = log.record(level, &done.message) {
            diagnostic(&format!(
                "deployment output log completion write failed: {error}"
            ));
        }
    }
    drop(guard);
    if let Some(ui) = ui {
        telemetry.finish(done.clone());
        ui.finish(done.clone());
    } else {
        // There is no window keeping this process alive after deployment.
        telemetry.finish_and_wait(done.clone());
    }
    done
}

/// 启动原生部署子进程，同时读取 stdout 与 stderr 并增量解码 UTF-8。
///
/// 子进程退出即视为部署完成，即使管道因继承句柄尚未 EOF 也不继续等待；退出前残余
/// 输出尽力读取。启动、路径发现或等待子进程失败时返回可展示的错误文本。
async fn run_process(
    publish: &mut impl FnMut(&str, String),
) -> Result<std::process::ExitStatus, String> {
    let paths = weasel_common::process::RuntimePaths::discover().map_err(|e| e.to_string())?;
    let exe = paths.executable_directory.join("weasel-server.exe");
    let mut child = tokio::process::Command::new(&exe)
        .arg("--deploy")
        .current_dir(&paths.executable_directory)
        .creation_flags(crate::bindings::CREATE_NO_WINDOW as u32)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动部署进程：{e}"))?;
    diagnostic(&format!("native deployer started pid={:?}", child.id()));
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_buffer = [0; 16384];
    let mut err_buffer = [0; 16384];
    let mut out_decoder = Utf8Stream::default();
    let mut err_decoder = Utf8Stream::default();
    let (mut out_eof, mut err_eof) = (false, false);
    let mut read_error = None;
    let status = loop {
        tokio::select! {
            biased;
            result = child.wait() => {
                let exit = result.map_err(|e| e.to_string())?;
                diagnostic(&format!("native deployer exited: {exit}"));
                // Process exit releases the native data lock; inherited pipe handles
                // must not delay completion. Trailing diagnostics are best effort.
                break exit;
            }
            read = stdout.read(&mut out_buffer), if !out_eof => {
                let count = match read {
                    Ok(count) => count,
                    Err(error) => { read_error = Some(format!("stdout read failed: {error}")); 0 }
                };
                let text = out_decoder.feed(&out_buffer[..count], count == 0);
                if !text.is_empty() { publish("stdout", text); }
                if count == 0 { out_eof = true; }
            }
            read = stderr.read(&mut err_buffer), if !err_eof => {
                let count = match read {
                    Ok(count) => count,
                    Err(error) => { read_error = Some(format!("stderr read failed: {error}")); 0 }
                };
                let text = err_decoder.feed(&err_buffer[..count], count == 0);
                if !text.is_empty() { publish("stderr", text); }
                if count == 0 { err_eof = true; }
            }

        }
    };
    if let Some(error) = read_error {
        diagnostic(&error);
    }
    Ok(status)
}

/// 缓存跨读取块的 UTF-8 字节，并保留尚未完整到达的字符。
#[derive(Default)]
struct Utf8Stream(Vec<u8>);
impl Utf8Stream {
    /// 解码新字节；非 EOF 时将末尾不完整字符留待下次输入。
    ///
    /// EOF 或无效 UTF-8 会通过有损转换输出可解码部分，保证缓存不会因坏字节而无限增长。
    fn feed(&mut self, bytes: &[u8], eof: bool) -> String {
        self.0.extend_from_slice(bytes);
        let usable = match std::str::from_utf8(&self.0) {
            Ok(_) => self.0.len(),
            Err(error) if error.error_len().is_none() && !eof => error.valid_up_to(),
            Err(_) => self.0.len(),
        };
        let text = String::from_utf8_lossy(&self.0[..usable]).into_owned();
        self.0.drain(..usable);
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_utf8_logs_are_preserved() {
        let expected = "部署日志\r\n🙂\n";
        let mut decoder = Utf8Stream::default();
        let mut actual = String::new();
        for byte in expected.as_bytes() {
            actual.push_str(&decoder.feed(&[*byte], false));
        }
        actual.push_str(&decoder.feed(&[], true));
        assert_eq!(actual, expected);
    }
    #[test]
    fn completion_survives_a_stalled_ui_and_log_overflow() {
        let ui = UiMailbox::default();
        for _ in 0..10000 {
            ui.log(&"中".repeat(1000));
        }
        let done = DeployComplete {
            success: true,
            exit_code: Some(0),
            message: "done".into(),
        };
        ui.finish(done.clone());
        let batch = ui.take();
        assert!(batch.omitted);
        assert!(batch.bytes <= PREVIEW_LIMIT);
        assert_eq!(batch.complete, Some(done));
    }
}
