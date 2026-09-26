//! 管理 TIP 与 TSF 线程管理器之间的激活、焦点订阅和停用清理。
//!
//! 注册过程按步骤建立 COM sink，并在中途失败时撤销已完成的订阅；清理过程先使回调失效，
//! 再以可重入安全的方式提取资源。若锁正被同步回调占用，资源保持原位并由公寓更新窗口重试。
use super::*;

impl TextService {
    /// 完成 TSF 激活：建立基础订阅、启动公寓更新窗口、尽力注册语言栏并订阅焦点上下文。
    ///
    /// 语言栏注册失败只记录警告，不阻止 TIP 输入；其他必要步骤失败则调用统一停用逻辑
    /// 回滚已建立的状态。`owner` 的 COM 引用用于保活服务对象及更新窗口回调目标。
    pub(super) fn activate_tsf(
        &self,
        thread_mgr: Ref<'_, ITfThreadMgr>,
        tid: TfClientId,
        owner: IUnknown,
        secure_mode: bool,
    ) -> Result<()> {
        let result = (|| {
            self.activate_with_thread_manager(
                thread_mgr,
                tid,
                owner.cast()?,
                owner.cast()?,
                secure_mode,
            )?;
            // Notifications must have a destination before creating context RPCs.
            self.start_update_window(owner.clone())?;
            let manager = self.lock(&self.thread_mgr)?.clone();
            if let Some(manager) = manager {
                match language_bar::LanguageBar::attach(&manager) {
                    Ok(bar) => *self.lock(&self.language_bar)? = Some(Arc::new(bar)),
                    Err(error) => self
                        .lock(&self.rpc)?
                        .log("warn", format!("language bar registration failed: {error}")),
                }
            }
            self.subscribe_to_focused_context(owner.cast()?, owner.cast()?)
        })();
        if result.is_err() {
            self.deactivate_internal();
        }
        result
    }

    /// 注册线程管理器、键盘和线程焦点 sink，并初始化本次激活的连接状态。
    ///
    /// 方法先清理旧激活，再依次订阅；任一后续注册失败都会撤销此前成功的订阅。只有资源
    /// 和初始输入/显示属性加载成功后才发布 `activated`。调用方需在 TSF 所属公寓中执行。
    pub(super) fn activate_with_thread_manager(
        &self,
        thread_mgr: Ref<'_, ITfThreadMgr>,
        tid: TfClientId,
        sink: ITfThreadMgrEventSink,
        key_sink: ITfKeyEventSink,
        secure_mode: bool,
    ) -> Result<()> {
        // TSF normally activates a TIP once per instance, but a reactivation
        // must not leave the previous sink registered on the thread manager.
        self.deactivate_internal();

        if self.teardown_pending.load(Ordering::Acquire) {
            return Err(Error::from_hresult(boundary::E_PENDING));
        }

        let Some(thread_mgr) = thread_mgr.to_owned() else {
            return Err(Error::from_hresult(E_POINTER));
        };
        let source: ITfSource = thread_mgr.cast()?;
        let cookie = unsafe { source.AdviseSink(&ITfThreadMgrEventSink::IID, &sink)? };
        let keystroke_mgr: ITfKeystrokeMgr = match thread_mgr.cast() {
            Ok(manager) => manager,
            Err(error) => {
                unsafe {
                    let _ = source.UnadviseSink(cookie);
                }
                return Err(error);
            }
        };
        let hr = unsafe { keystroke_mgr.AdviseKeyEventSink(tid, &key_sink, true) };
        if hr.is_err() {
            unsafe {
                let _ = source.UnadviseSink(cookie);
            }
            return Err(Error::from_hresult(hr));
        }

        let focus_cookie = match unsafe { source.AdviseSink(&ITfThreadFocusSink::IID, &sink) } {
            Ok(cookie) => cookie,
            Err(error) => {
                unsafe {
                    let _ = keystroke_mgr.UnadviseKeyEventSink(tid);
                    let _ = source.UnadviseSink(cookie);
                }
                return Err(error);
            }
        };
        *self.lock(&self.thread_focus_sink_cookie)? = Some(focus_cookie);
        *self.lock(&self.thread_mgr)? = Some(thread_mgr);
        *self.lock(&self.thread_mgr_event_sink_cookie)? = Some(cookie);
        *self.lock(&self.keystroke_mgr)? = Some(keystroke_mgr);
        *self.lock(&self.keystroke_client_id)? = Some(tid);
        self.lock(&self.rpc)?.start();
        self.secure_mode.store(secure_mode, Ordering::Release);
        self.allow_rime_in_secure_fields
            .store(false, Ordering::Release);
        self.secure_policy_epoch.store(0, Ordering::Release);
        self.load_input_mode()?;
        self.register_display_attribute()?;
        self.activated.store(true, Ordering::Release);
        Ok(())
    }

    /// 根据焦点文档的栈顶上下文建立或清除文本编辑订阅，并更新服务焦点状态。
    ///
    /// `document_mgr == None` 或无法取得栈顶上下文时会清除焦点；取得上下文后安排安全字段
    /// 探测。TSF 接口调用失败会向上返回，供外层回调边界统一转换和隔离。
    pub(super) fn set_text_edit_sink(
        &self,
        document_mgr: Option<ITfDocumentMgr>,
        edit_sink: ITfTextEditSink,
        _layout_sink: ITfTextLayoutSink,
    ) -> Result<()> {
        let context = document_mgr.and_then(|manager| unsafe { manager.GetTop().ok() });
        let next = match context {
            Some(context) => Some(self.ensure_context(context, &edit_sink.cast()?)?),
            None => None,
        };
        self.focus_context(next.clone())?;
        if let Some(state) = next {
            self.request_secure_field_probe(&state, edit_sink.cast()?, false)?;
        }
        Ok(())
    }

    /// 从线程管理器重新读取焦点文档，并把焦点同步到其栈顶上下文。
    pub(super) fn subscribe_to_focused_context(
        &self,
        edit_sink: ITfTextEditSink,
        layout_sink: ITfTextLayoutSink,
    ) -> Result<()> {
        let thread_mgr = self.lock(&self.thread_mgr)?.clone();
        let document_mgr = thread_mgr.and_then(|thread_mgr| unsafe { thread_mgr.GetFocus().ok() });
        self.set_text_edit_sink(document_mgr, edit_sink, layout_sink)?;
        Ok(())
    }

    /// 使回调失效并释放本次激活持有的 TSF、RPC、上下文和窗口资源。
    ///
    /// 可能从 TSF 回调或更新窗口重入，因此以原子标志阻止并行清理。先停止全部上下文，
    /// 再一次性尝试取得清理所需的锁；任何锁冲突都会保留全部资源并返回，以便更新窗口重试。
    /// 外部注销和析构操作逐项隔离，单项失败不妨碍其余清理。
    pub(super) fn deactivate_internal(&self) {
        // 在任何可能触发同步重入的外部调用前，使服务回调失效。
        self.activated.store(false, Ordering::Release);
        if self.tearing_down.swap(true, Ordering::AcqRel) {
            return;
        }
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _reset = Reset(&self.tearing_down);
        if !self.teardown_pending.swap(true, Ordering::AcqRel) {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        self.faulted.request_maintenance();
        use boundary::{cleanup, try_teardown};
        let states = match try_teardown(&self.contexts) {
            Some(states) => states.clone(),
            None => return,
        };
        for state in &states {
            if !state.stop() {
                return;
            }
        }
        // 先取得全部服务锁再提取资源；重入导致锁不可用时，所有资源仍由服务持有，
        // 后续由 TSF 公寓定时窗口重试。
        let extracted = (|| {
            let mut bar = try_teardown(&self.language_bar)?;
            let mut rpc = try_teardown(&self.rpc)?;
            let mut window = try_teardown(&self.update_window)?;
            let mut tested = try_teardown(&self.tested_key)?;
            let mut pending = try_teardown(&self.pending_edit)?;
            let mut atom = try_teardown(&self.display_attribute_atom)?;
            let mut focus = try_teardown(&self.focused_context)?;
            let mut contexts = try_teardown(&self.contexts)?;
            let mut keys = try_teardown(&self.keystroke_mgr)?;
            let mut tid = try_teardown(&self.keystroke_client_id)?;
            let mut cookie = try_teardown(&self.thread_mgr_event_sink_cookie)?;
            let mut focus_cookie = try_teardown(&self.thread_focus_sink_cookie)?;
            let mut manager = try_teardown(&self.thread_mgr)?;
            tested.take();
            atom.take();
            focus.take();
            Some((
                bar.take(),
                std::mem::take(&mut *rpc),
                window.take(),
                std::mem::take(&mut *pending),
                std::mem::take(&mut *contexts),
                keys.take(),
                tid.take(),
                cookie.take(),
                focus_cookie.take(),
                manager.take(),
            ))
        })();
        let Some((
            bar,
            mut rpc,
            window,
            pending,
            contexts,
            keys,
            tid,
            cookie,
            focus_cookie,
            manager,
        )) = extracted
        else {
            return;
        };
        self.edit_requested.store(false, Ordering::Release);
        cleanup(|| drop(bar));
        cleanup(|| rpc.stop());
        cleanup(|| drop(pending));
        cleanup(|| drop(contexts));
        cleanup(|| {
            if let (Some(keys), Some(tid)) = (keys, tid) {
                unsafe {
                    let _ = keys.UnadviseKeyEventSink(tid);
                }
            }
        });
        cleanup(|| {
            if let (Some(manager), Some(cookie)) = (manager, cookie) {
                if let Ok(source) = manager.cast::<ITfSource>() {
                    unsafe {
                        let _ = source.UnadviseSink(cookie);
                        if let Some(focus_cookie) = focus_cookie {
                            let _ = source.UnadviseSink(focus_cookie);
                        }
                    }
                }
            }
        });
        self.teardown_pending.store(false, Ordering::Release);
        self.faulted.set_window(0);
        cleanup(|| drop(window));
    }

    /// 在 TSF 公寓创建维护窗口，并将其句柄发布给服务及各上下文 RPC。
    ///
    /// 窗口回调与创建、销毁均在同一公寓执行。闭包通过服务对象的原始地址访问状态，
    /// 其有效期由捕获的 COM `session` 引用保证；停用必须先完成清理并销毁窗口，才能释放
    /// 其引用的服务状态。定时回调负责重试停用、清理失效上下文、同步安全字段及语言栏，
    /// 并安排 TSF 编辑事务；每轮错误由边界守卫隔离。
    pub(super) fn start_update_window(&self, session: IUnknown) -> Result<()> {
        let service = self as *const TextService;
        // 捕获的 COM 引用保活服务分配；窗口在 TSF 公寓创建、派发和销毁，停用会在释放
        // 服务状态前显式销毁窗口。
        let window = update_window::UpdateWindow::new(move || unsafe {
            let service = &*service;
            if service.teardown_pending.load(Ordering::Acquire) {
                service.deactivate_internal();
                return;
            }
            service.faulted.retry_report();
            if let Ok(states) = service.lock(&service.contexts).map(|states| states.clone()) {
                for state in states {
                    if !state.alive.load(Ordering::Acquire) {
                        let _ = service.remove_context(&state.context);
                    }
                }
            }
            if service.faulted.load(Ordering::Acquire) {
                if boundary::guard(None, || service.refresh_language_bar()).is_err() {
                    service.faulted.request_maintenance();
                }
                return;
            }
            let _ = boundary::guard(Some(&service.faulted), || {
                if !service.activated.load(Ordering::Acquire) {
                    return Ok(());
                }
                service.drain_context_updates(&session)?;
                service.reconcile_secure_field()?;
                service.refresh_language_bar()?;
                service.schedule_edit()?;
                Ok(())
            });
        })?;
        self.faulted.set_window(window.hwnd.0 as usize);
        self.lock(&self.rpc)?
            .set_update_window(window.hwnd.0 as usize);
        let states = self.lock(&self.contexts)?.clone();
        for state in states {
            self.lock(&state.rpc)?
                .set_update_window(window.hwnd.0 as usize);
        }
        *self.lock(&self.update_window)? = Some(window);
        Ok(())
    }
}
