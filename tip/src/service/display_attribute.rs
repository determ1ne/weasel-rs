use super::*;

#[implement(ITfDisplayAttributeInfo)]
pub(super) struct DisplayAttributeInfo {
    _module: ModuleLease,
}

impl DisplayAttributeInfo {
    pub(super) fn new() -> Self {
        Self {
            _module: ModuleLease::new(),
        }
    }

    fn attribute() -> TF_DISPLAYATTRIBUTE {
        let default_color = || TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        };
        TF_DISPLAYATTRIBUTE {
            crText: default_color(),
            crBk: default_color(),
            lsStyle: TF_LS_DOT,
            fBoldLine: BOOL(0),
            crLine: default_color(),
            bAttr: TF_ATTR_INPUT,
        }
    }
}

impl ITfDisplayAttributeInfo_Impl for DisplayAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        boundary::guard(None, || Ok(GUID_WEASEL_DISPLAY_ATTRIBUTE))
    }

    fn GetDescription(&self) -> Result<windows_core::BSTR> {
        boundary::guard(None, || {
            Ok(windows_core::BSTR::from("weasel-rs composition input"))
        })
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        boundary::guard(None, || {
            if pda.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            unsafe {
                *pda = DisplayAttributeInfo::attribute();
            }
            Ok(())
        })
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        boundary::guard(None, || not_implemented())
    }

    fn Reset(&self) -> Result<()> {
        boundary::guard(None, || not_implemented())
    }
}

#[implement(IEnumTfDisplayAttributeInfo)]
pub(super) struct DisplayAttributeEnumerator {
    index: AtomicU32,
    _module: ModuleLease,
}

impl DisplayAttributeEnumerator {
    pub(super) fn new() -> Self {
        Self {
            index: AtomicU32::new(0),
            _module: ModuleLease::new(),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for DisplayAttributeEnumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        boundary::guard(None, || {
            Ok(DisplayAttributeEnumerator {
                index: AtomicU32::new(self.index.load(Ordering::Acquire)),
                _module: ModuleLease::new(),
            }
            .into())
        })
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: windows_core::OutRef<'_, ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        boundary::guard(None, || {
            if !pcfetched.is_null() {
                unsafe {
                    *pcfetched = 0;
                }
            }
            if ulcount == 0 {
                return Ok(());
            }

            if self
                .index
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |index| {
                    (index == 0).then_some(1)
                })
                .is_ok()
            {
                rginfo.write(Some(DisplayAttributeInfo::new().into()))?;
                if !pcfetched.is_null() {
                    unsafe {
                        *pcfetched = 1;
                    }
                }
                if ulcount == 1 {
                    return Ok(());
                }
            }

            // IEnumTfDisplayAttributeInfo has only one input attribute.  A
            // partial or exhausted enumeration is reported as S_FALSE, matching
            // the Weasel and Mozc implementations.
            Err(Error::from_hresult(S_FALSE))
        })
    }

    fn Reset(&self) -> Result<()> {
        boundary::guard(None, || {
            self.index.store(0, Ordering::Release);
            Ok(())
        })
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        boundary::guard(None, || {
            if ulcount != 0 {
                self.index.store(1, Ordering::Release);
            }
            Ok(())
        })
    }
}

impl TextService {
    pub(super) fn register_display_attribute(&self) -> Result<()> {
        let category_mgr = match unsafe {
            bindings::CoCreateInstance::<_, ITfCategoryMgr>(
                &CLSID_TF_CategoryMgr,
                None,
                CLSCTX_INPROC_SERVER,
            )
        } {
            Ok(category_mgr) => category_mgr,
            Err(error) => {
                self.lock(&self.rpc)?.log(
                    "error",
                    format!("failed to create TSF category manager: {error}"),
                );
                return Ok(());
            }
        };

        match unsafe { category_mgr.RegisterGUID(&GUID_WEASEL_DISPLAY_ATTRIBUTE) } {
            Ok(atom) => {
                *self.lock(&self.display_attribute_atom)? = Some(atom);
            }
            Err(error) => {
                self.lock(&self.rpc)?.log(
                    "error",
                    format!("failed to register composition display attribute: {error}"),
                );
            }
        }
        Ok(())
    }

    pub(super) fn set_display_attribute(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) -> Result<()> {
        let Some(atom) = *self.lock(&self.display_attribute_atom)? else {
            return Ok(());
        };
        let property: ITfProperty = match unsafe { context.GetProperty(&GUID_PROP_ATTRIBUTE) } {
            Ok(property) => property,
            Err(error) => {
                self.lock(&self.rpc)?.log(
                    "warn",
                    format!("display attribute GetProperty failed: {error:?}"),
                );
                return Err(error);
            }
        };
        let variant = VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                    vt: VARTYPE(VT_I4 as u16),
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 {
                        lVal: atom.0 as i32,
                    },
                }),
            },
        };
        let hr = unsafe { property.SetValue(ec, range, &variant) };
        if hr.is_ok() {
            Ok(())
        } else {
            let error = Error::from_hresult(hr);
            self.lock(&self.rpc)?.log(
                "warn",
                format!("display attribute SetValue failed: {error:?}"),
            );
            Err(error)
        }
    }

    pub(super) fn clear_display_attribute(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) -> Result<()> {
        if self.lock(&self.display_attribute_atom)?.is_none() {
            return Ok(());
        }
        let property: ITfProperty = match unsafe { context.GetProperty(&GUID_PROP_ATTRIBUTE) } {
            Ok(property) => property,
            Err(error) => {
                self.lock(&self.rpc)?.log(
                    "warn",
                    format!("display attribute GetProperty for Clear failed: {error:?}"),
                );
                return Err(error);
            }
        };
        let hr = unsafe { property.Clear(ec, range) };
        if hr.is_ok() {
            Ok(())
        } else {
            let error = Error::from_hresult(hr);
            self.lock(&self.rpc)?
                .log("warn", format!("display attribute Clear failed: {error:?}"));
            Err(error)
        }
    }

    pub(super) fn set_display_attribute_best_effort(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) {
        let _ = self.set_display_attribute(context, ec, range);
    }

    pub(super) fn clear_display_attribute_best_effort(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) {
        let _ = self.clear_display_attribute(context, ec, range);
    }
}
