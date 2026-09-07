//! A bounded mailbox whose processor is constructed and destroyed on one OS thread.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub(crate) struct Sender<T> {
    queue: mpsc::SyncSender<T>,
    wake: thread::Thread,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            wake: self.wake.clone(),
        }
    }
}
impl<T> Sender<T> {
    pub fn try_send(&self, message: T) -> Result<(), mpsc::TrySendError<T>> {
        let result = self.queue.try_send(message);
        self.wake.unpark();
        result
    }
    pub fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let thread = self.wake.clone();
        Arc::new(move || thread.unpark())
    }
}

pub(crate) trait Processor<T> {
    fn process(&mut self, message: T);
    fn idle(&mut self) {}
}

pub(crate) struct Worker<T> {
    pub sender: Sender<T>,
    pub ready: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    pub finished: tokio::sync::oneshot::Receiver<()>,
}

impl<T: Send + 'static> Worker<T> {
    pub fn spawn<S: Processor<T> + 'static>(
        capacity: usize,
        create: impl FnOnce() -> Result<S, String> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let ready = Arc::new(AtomicBool::new(false));
        let readiness = ready.clone();
        let (done, finished) = tokio::sync::oneshot::channel();
        let thread = thread::Builder::new()
            .name("rime-engine".into())
            .spawn(move || {
                let result = (|| {
                    // S deliberately has no Send bound: all native state originates here.
                    let mut processor = create()?;
                    readiness.store(true, Ordering::Release);
                    while !stopping.load(Ordering::Acquire) {
                        match receiver.try_recv() {
                            Ok(message) => {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                processor.process(message);
                            }
                            Err(mpsc::TryRecvError::Empty) => {
                                processor.idle();
                                if !stopping.load(Ordering::Acquire) {
                                    thread::park();
                                }
                                continue;
                            }
                            Err(mpsc::TryRecvError::Disconnected) => break,
                        }
                        processor.idle();
                    }
                    // Drop sessions and finalize before reporting completion.
                    readiness.store(false, Ordering::Release);
                    drop(processor);
                    Ok(())
                })();
                let _ = done.send(());
                result
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Sender {
                queue: sender,
                wake: thread.thread().clone(),
            },
            ready,
            stop,
            thread: Some(thread),
            finished,
        })
    }

    pub fn request_stop(&self) {
        self.ready.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.sender.wake.unpark();
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        self.request_stop();
        let thread = self.thread.take().expect("worker joined once");
        tokio::task::spawn_blocking(move || {
            thread
                .join()
                .map_err(|_| "engine thread panicked".to_owned())?
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.ready.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.sender.wake.unpark();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use std::{rc::Rc, sync::Mutex};

    #[tokio::test]
    async fn idle_engine_sleeps_until_explicit_wake_and_shutdown_needs_no_queue_slot() {
        struct Idle(mpsc::Sender<()>);
        impl Processor<()> for Idle {
            fn process(&mut self, _: ()) {}
            fn idle(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (tx, rx) = mpsc::channel();
        let worker = Worker::spawn(1, move || Ok(Idle(tx))).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
        (worker.sender.notifier())();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.shutdown().await.unwrap();
    }

    struct Probe {
        owner: thread::ThreadId,
        _not_send: Rc<()>,
        events: Arc<Mutex<Vec<&'static str>>>,
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: mpsc::Receiver<()>,
    }
    impl Processor<()> for Probe {
        fn process(&mut self, _: ()) {
            assert_eq!(thread::current().id(), self.owner);
            self.events.lock().unwrap().push("process");
            let _ = self.entered.take().unwrap().send(());
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            assert_eq!(thread::current().id(), self.owner);
            self.events.lock().unwrap().push("finalize");
        }
    }

    #[tokio::test]
    async fn bounded_queue_and_shutdown_preserve_thread_ownership() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let caller = thread::current().id();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = mpsc::channel();
        let worker = Worker::spawn(1, move || {
            let owner = thread::current().id();
            assert_ne!(caller, owner);
            recorded.lock().unwrap().push("initialize");
            Ok(Probe {
                owner,
                _not_send: Rc::new(()),
                events: recorded,
                entered: Some(entered),
                release: blocked,
            })
        })
        .unwrap();
        worker.sender.try_send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        worker.sender.try_send(()).unwrap();
        assert!(matches!(
            worker.sender.try_send(()),
            Err(mpsc::TrySendError::Full(()))
        ));
        // Shutdown bypasses the full mailbox and discards queued input.
        worker.request_stop();
        release.send(()).unwrap();
        worker.shutdown().await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["initialize", "process", "finalize"]
        );
    }

    #[tokio::test]
    async fn startup_failure_is_reported_and_joined() {
        let mut worker =
            Worker::<()>::spawn::<Probe>(1, || Err("fake startup failure".into())).unwrap();
        (&mut worker.finished).await.unwrap();
        assert_eq!(worker.shutdown().await.unwrap_err(), "fake startup failure");
    }
}
