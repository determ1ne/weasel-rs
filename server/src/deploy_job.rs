//! Deployment runs independently of the UI. Only a bounded preview is shared.
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::AsyncReadExt;
use weasel_common::{deploy_protocol::DeployComplete, process::SingleInstance};

pub const PREVIEW_LIMIT: usize = 256 * 1024;

#[derive(Default)]
pub struct Preview {
    pub chunks: VecDeque<String>,
    bytes: usize,
    pub omitted: bool,
    pub complete: Option<DeployComplete>,
}

#[derive(Default)]
pub struct UiMailbox {
    pending: Mutex<Preview>,
    window: AtomicUsize,
}

impl UiMailbox {
    pub fn set_window(&self, hwnd: usize) {
        self.window.store(hwnd, Ordering::Release);
    }
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
    }
    pub fn finish(&self, done: DeployComplete) {
        self.pending.lock().unwrap().complete = Some(done);
        let hwnd = self.window.load(Ordering::Acquire);
        if hwnd != 0 {
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
                diagnostic("could not post completion to UI; timer will consume it");
            }
        }
    }
    pub fn take(&self) -> Preview {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
}

pub fn diagnostic(message: &str) {
    tracing::info!(message, "deployment diagnostic");
}

pub fn run(ui: Option<Arc<UiMailbox>>, guard: SingleInstance) -> DeployComplete {
    diagnostic("deployment worker started");
    let telemetry = crate::deploy_telemetry::Telemetry::stdout();
    let mut publish = |source: &str, text: String| {
        // Native output is preview/telemetry only; persistent diagnostics are bounded
        // by the ComponentLogger installed by main for this deployment process.
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

async fn run_process(
    publish: &mut impl FnMut(&str, String),
) -> Result<std::process::ExitStatus, String> {
    let paths =
        weasel_common::runtime_paths::RuntimePaths::discover().map_err(|e| e.to_string())?;
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

#[derive(Default)]
struct Utf8Stream(Vec<u8>);
impl Utf8Stream {
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
