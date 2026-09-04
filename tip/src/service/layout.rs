use super::*;

#[implement(ITfEditSession)]
pub(super) struct LayoutProbe {
    pub(super) view: ITfContextView,
    pub(super) range: ITfRange,
    pub(super) state: Arc<ContextState>,
    pub(super) token: weasel_common::message::ContextToken,
    pub(super) generation: Arc<AtomicU64>,
    pub(super) requested_generation: u64,
    pub(super) _module: ModuleLease,
}

impl ITfEditSession_Impl for LayoutProbe_Impl {
    fn DoEditSession(&self, ec: TfEditCookie) -> Result<()> {
        boundary::guard(None, || {
            if !self.state.matches(Some(&self.token))? {
                return Ok(());
            }
            if self.generation.load(Ordering::Acquire) != self.requested_generation {
                return Ok(());
            }
            let mut rect = RECT::default();
            let mut clipped = BOOL(0);
            let hr = unsafe {
                self.view
                    .GetTextExt(ec, &self.range, &mut rect, &mut clipped)
            };
            if hr.is_err() {
                self.state
                    .rpc
                    .lock()
                    .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                    .log(
                        "debug",
                        format!("layout anchor GetTextExt failed hr={hr:?}"),
                    );
                return Ok(());
            }
            if self.generation.load(Ordering::Acquire) != self.requested_generation {
                return Ok(());
            }
            self.state
                .rpc
                .lock()
                .map_err(|_| Error::from_hresult(boundary::E_FAIL))?
                .send_layout_update(LayoutUpdate {
                    session_id: self.state.id,
                    token: Some(self.token.clone()),
                    anchor: Some(RenderRect {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                        valid: rect.right > rect.left && rect.bottom > rect.top,
                    }),
                });
            Ok(())
        })
    }
}
