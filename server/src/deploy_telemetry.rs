//! 通过 stdout 尽力发送部署日志和完成事件。
//!
//! 日志走有界队列，拥塞时允许丢弃；完成事件使用独立槽位并优先于尚未发送的日志。
//! 无界面模式只有限等待完成事件写出，写线程不执行 join，以免继承的输出管道永久阻塞。
use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use weasel_common::deploy_protocol::{
    DeployComplete, DeployEvent, DeployLog, Payload, write_event,
};

/// 将部署日志与完成事件序列化写入事件流。
///
/// 日志发送不会阻塞部署任务，输出线程独立运行并故意分离；因此外部管道卡住时只会
/// 影响遥测写出，不会卡住部署本身。完成事件不与日志争用队列容量。
pub(crate) struct Telemetry {
    /// 有界日志队列；满载时新日志会被静默丢弃。
    logs: mpsc::SyncSender<DeployLog>,
    /// 完成事件的单独槽位，后写入的结果覆盖此前尚未发送的结果。
    completion: Arc<Mutex<Option<DeployComplete>>>,
    /// 写线程成功写出完成事件后的通知。
    finished: mpsc::Receiver<()>,
    /// 输出线程唤醒句柄；线程创建失败时为空，遥测将无法发送。
    wake: Option<std::thread::Thread>,
    /// 生命周期结束标志，令空闲写线程退出。
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl Telemetry {
    /// 创建写入标准输出的遥测发送器。
    pub fn stdout() -> Self {
        Self::spawn_writer(64, |event| {
            write_event(&mut std::io::stdout().lock(), event)
        })
    }

    /// 启动后台写线程并返回非阻塞发送端。
    ///
    /// 写入闭包必须可跨线程调用。队列容量限制待发日志数；线程创建失败不会使部署
    /// 初始化失败，但后续完成等待会在超时后返回。后台线程不会被连接或强制等待。
    fn spawn_writer<E: std::fmt::Display>(
        capacity: usize,
        mut write: impl FnMut(&DeployEvent) -> Result<(), E> + Send + 'static,
    ) -> Self {
        let (logs, receiver) = mpsc::sync_channel(capacity);
        let completion = Arc::new(Mutex::new(None));
        let pending = completion.clone();
        let (finished_sender, finished) = mpsc::sync_channel(1);
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = closed.clone();
        let spawned = std::thread::Builder::new()
            .name("deploy-telemetry".into())
            .spawn(move || {
                loop {
                    let done = pending.lock().unwrap().take();
                    if let Some(done) = done {
                        let result = write(&DeployEvent {
                            payload: Some(Payload::Complete(done)),
                        });
                        if result.is_ok() {
                            let _ = finished_sender.send(());
                        }
                        break;
                    }
                    let log = match receiver.try_recv() {
                        Ok(log) => log,
                        Err(mpsc::TryRecvError::Empty) => {
                            if stopped.load(std::sync::atomic::Ordering::Acquire) {
                                if pending.lock().unwrap().is_some() {
                                    continue;
                                }
                                break;
                            }
                            std::thread::park();
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            if pending.lock().unwrap().is_some() {
                                continue;
                            }
                            break;
                        }
                    };
                    // Completion has its own slot and supersedes queued log chunks.
                    if pending.lock().unwrap().is_some() {
                        continue;
                    }
                    if write(&DeployEvent {
                        payload: Some(Payload::Log(log)),
                    })
                    .is_err()
                    {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            tracing::warn!("deployment telemetry thread unavailable");
        }
        // Intentionally detach: an inherited stdout handle can block forever.
        Self {
            wake: spawned.as_ref().ok().map(|thread| thread.thread().clone()),
            closed,
            logs,
            completion,
            finished,
        }
    }

    /// 尝试排队一条日志并唤醒写线程。
    ///
    /// 队列已满或接收端关闭时日志丢弃；此操作不等待输出管道可写。
    pub fn log(&self, stream: &str, text: String) {
        let _ = self.logs.try_send(DeployLog {
            stream: stream.into(),
            text,
        });
        if let Some(thread) = &self.wake {
            thread.unpark();
        }
    }
    /// 发布优先级高于日志的完成事件，并唤醒写线程。
    ///
    /// 完成槽位容量为一；尚未写出的完成结果会被新结果替换。
    pub fn finish(&self, done: DeployComplete) {
        *self.completion.lock().unwrap() = Some(done);
        if let Some(thread) = &self.wake {
            thread.unpark();
        }
    }

    /// 发布完成事件，并最多等待两秒确认其写出。
    ///
    /// 超时只记录警告，不连接写线程；继承的 stdout 管道可能永久阻塞后台写入。
    pub fn finish_and_wait(&self, done: DeployComplete) {
        self.finish(done);
        // An inherited pipe can stall forever; never join its writer thread.
        if self.finished.recv_timeout(Duration::from_secs(2)).is_err() {
            tracing::warn!("deployment completion could not be flushed before exit");
        }
    }
}

impl Drop for Telemetry {
    /// 标记关闭并唤醒空闲写线程；不等待可能阻塞于输出操作的线程。
    fn drop(&mut self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = &self.wake {
            thread.unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_wait_is_bounded_when_the_output_writer_stalls() {
        let (release, blocked) = mpsc::channel();
        let (entered, waiting) = mpsc::channel();
        let telemetry = Telemetry::spawn_writer(1, move |_| -> std::io::Result<()> {
            entered.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        });
        telemetry.finish(DeployComplete::default());
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            telemetry.finished.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        release.send(()).unwrap();
        telemetry
            .finished
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
    #[test]
    fn headless_completion_is_written_before_returning() {
        let (sent, received) = mpsc::channel();
        let telemetry = Telemetry::spawn_writer(1, move |event| -> std::io::Result<()> {
            sent.send(event.clone()).unwrap();
            Ok(())
        });
        let done = DeployComplete {
            success: true,
            exit_code: Some(0),
            message: "done".into(),
        };
        telemetry.finish_and_wait(done.clone());
        assert_eq!(
            received.try_recv().unwrap().payload,
            Some(Payload::Complete(done))
        );
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn blocked_stdout_does_not_block_completion_or_ui_and_skips_queued_logs() {
        let (entered, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (delivered, completed) = mpsc::channel();
        let mut first = true;
        let telemetry = Telemetry::spawn_writer(1, move |event| -> std::io::Result<()> {
            if first {
                first = false;
                entered.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            } else {
                delivered.send(event.clone()).unwrap();
            }
            Ok(())
        });
        telemetry.log("stderr", "first".into());
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..10000 {
            telemetry.log("stderr", "queued or dropped".into());
        }
        let done = DeployComplete {
            success: true,
            exit_code: Some(0),
            message: "done".into(),
        };
        telemetry.finish(done.clone());
        let ui = crate::deploy_job::UiMailbox::default();
        ui.finish(done.clone());
        assert_eq!(ui.take().complete, Some(done.clone()));
        release.send(()).unwrap();
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .payload,
            Some(Payload::Complete(done))
        );
    }
}
