//! 提供有界消息邮箱与专属工作线程。
//!
//! 处理器在工作线程内创建、调用并销毁，因此处理器本身不必实现 `Send`；这用于确保
//! 原生引擎状态及其会话始终由同一线程持有。邮箱满时发送方立即得到错误，停止请求
//! 则通过原子标志和线程唤醒独立传递，不占用队列容量。
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

/// 可克隆的有界邮箱发送端，同时持有唤醒工作线程所需的句柄。
pub(crate) struct Sender<T> {
    /// 满载时不会阻塞调用方的同步队列。
    queue: mpsc::SyncSender<T>,
    /// 与处理器同属的工作线程；发送及显式通知都通过它解除休眠。
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
    /// 尝试投递一条消息，并唤醒工作线程。
    ///
    /// 队列满或接收端已关闭时返回原消息；此方法不会等待队列腾出空间。
    pub fn try_send(&self, message: T) -> Result<(), mpsc::TrySendError<T>> {
        let result = self.queue.try_send(message);
        self.wake.unpark();
        result
    }
    /// 创建可跨线程调用的唤醒回调，不向邮箱写入消息。
    ///
    /// 可用于通知处理器有外部状态变化；回调只执行 `unpark`，不等待工作线程处理。
    pub fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let thread = self.wake.clone();
        Arc::new(move || thread.unpark())
    }
}

/// 在工作线程中独占使用的消息处理器。
///
/// 实现类型无需 `Send`：它由 `Worker::spawn` 的构造闭包直接在工作线程创建，并在同一
/// 线程依次接收 `process`、`idle` 调用及最终析构。
pub(crate) trait Processor<T> {
    /// 处理一条已从邮箱取出的消息。
    fn process(&mut self, message: T);
    /// 每轮取消息或空闲轮询后执行维护工作；无额外需求时无需重载。
    fn idle(&mut self) {}
}

/// 管理一个有界邮箱及其专属处理线程。
///
/// 释放或显式关闭都会请求停止并等待线程退出。成功启动后，`ready` 表示处理器已经
/// 创建且尚未进入关闭阶段；`finished` 在工作线程完成处理器析构后通知。
pub(crate) struct Worker<T> {
    /// 供其他线程投递消息的发送端。
    pub sender: Sender<T>,
    /// 处理器可用状态，使用 Release/Acquire 配对发布和读取。
    pub ready: Arc<AtomicBool>,
    /// 与队列容量无关的停止标志。
    stop: Arc<AtomicBool>,
    /// 尚未连接的线程句柄；关闭或析构时只取出并等待一次。
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    /// 工作线程结束通知；若处理器已创建，会先在该线程析构处理器再发送通知。
    pub finished: tokio::sync::oneshot::Receiver<()>,
}

impl<T: Send + 'static> Worker<T> {
    /// 创建有界邮箱并启动专属线程。
    ///
    /// 构造闭包和消息必须可发送到新线程，但处理器 `S` 不要求 `Send`。容量限制待处理
    /// 消息数；处理器初始化失败会通过 `finished` 报告线程已结束，并由 `shutdown` 返回
    /// 原始错误。线程创建失败则直接返回错误。
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

    /// 请求停止并唤醒线程；不等待当前处理中的消息完成。
    ///
    /// 停止标志独立于有界邮箱，因此即使队列已满也能关闭；尚未处理的排队消息会被丢弃。
    pub fn request_stop(&self) {
        self.ready.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.sender.wake.unpark();
    }

    /// 请求停止并异步等待线程退出，返回处理器初始化或线程执行错误。
    ///
    /// 阻塞式线程连接被放到 Tokio 的阻塞任务池中，避免占用当前异步执行线程。
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
    /// 在同步析构路径上请求停止并连接线程；处理器正在执行时析构可能等待其返回。
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
