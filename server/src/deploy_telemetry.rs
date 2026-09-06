//! Best-effort stdout transport. Headless exit waits briefly for completion,
//! but never joins a writer that could be stuck on an inherited pipe.
use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use weasel_common::deploy_protocol::{
    DeployComplete, DeployEvent, DeployLog, Payload, write_event,
};

pub(crate) struct Telemetry {
    logs: mpsc::SyncSender<DeployLog>,
    completion: Arc<Mutex<Option<DeployComplete>>>,
    finished: mpsc::Receiver<()>,
}

impl Telemetry {
    pub fn stdout() -> Self {
        Self::spawn_writer(64, |event| {
            write_event(&mut std::io::stdout().lock(), event)
        })
    }

    fn spawn_writer<E: std::fmt::Display>(
        capacity: usize,
        mut write: impl FnMut(&DeployEvent) -> Result<(), E> + Send + 'static,
    ) -> Self {
        let (logs, receiver) = mpsc::sync_channel(capacity);
        let completion = Arc::new(Mutex::new(None));
        let pending = completion.clone();
        let (finished_sender, finished) = mpsc::sync_channel(1);
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
                    let log = match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(log) => log,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
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
            logs,
            completion,
            finished,
        }
    }

    pub fn log(&self, stream: &str, text: String) {
        let _ = self.logs.try_send(DeployLog {
            stream: stream.into(),
            text,
        });
    }
    pub fn finish(&self, done: DeployComplete) {
        *self.completion.lock().unwrap() = Some(done);
    }

    pub fn finish_and_wait(&self, done: DeployComplete) {
        self.finish(done);
        // An inherited pipe can stall forever; never join its writer thread.
        if self.finished.recv_timeout(Duration::from_secs(2)).is_err() {
            tracing::warn!("deployment completion could not be flushed before exit");
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
