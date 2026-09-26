//! 为 TSF 组合文本提供显示属性，并把该属性应用到宿主文本范围。
use super::*;

#[implement(ITfDisplayAttributeInfo)]
/// 实现 TSF 的显示属性信息接口；该属性只读，描述当前输入组合。
pub(super) struct DisplayAttributeInfo {
    /// 保持模块加载，避免 COM 对象存活时卸载实现代码。
    _module: ModuleLease,
}

impl DisplayAttributeInfo {
    /// 创建独立的 COM 显示属性对象。
    pub(super) fn new() -> Self {
        Self {
            _module: ModuleLease::new(),
        }
    }

    /// 构造 TSF 使用的输入属性样式；颜色沿用宿主默认值并显示点状下划线。
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
    /// 返回此属性在服务注册时使用的稳定 GUID。
    fn GetGUID(&self) -> Result<GUID> {
        boundary::guard(None, || Ok(GUID_WEASEL_DISPLAY_ATTRIBUTE))
    }

    /// 返回供 TSF 属性 UI 使用的说明文本。
    fn GetDescription(&self) -> Result<windows_core::BSTR> {
        boundary::guard(None, || {
            Ok(windows_core::BSTR::from("weasel-rs composition input"))
        })
    }

    /// 将属性值写入 TSF 提供的输出指针；空指针作为 COM 参数错误返回。
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

    /// 属性由服务固定定义，不接受 TSF 对其进行修改。
    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        boundary::guard(None, || not_implemented())
    }

    /// 固定属性没有可重置的用户配置。
    fn Reset(&self) -> Result<()> {
        boundary::guard(None, || not_implemented())
    }
}

#[implement(IEnumTfDisplayAttributeInfo)]
/// 枚举本服务唯一的输入显示属性，游标可原子地供 COM 调用访问。
pub(super) struct DisplayAttributeEnumerator {
    /// `0` 表示尚未返回属性，`1` 表示已耗尽。
    index: AtomicU32,
    /// 枚举器存活期间保持实现模块加载。
    _module: ModuleLease,
}

impl DisplayAttributeEnumerator {
    /// 创建位于首项之前的枚举器。
    pub(super) fn new() -> Self {
        Self {
            index: AtomicU32::new(0),
            _module: ModuleLease::new(),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for DisplayAttributeEnumerator_Impl {
    /// 克隆当前游标位置；克隆后的枚举器独立推进。
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        boundary::guard(None, || {
            Ok(DisplayAttributeEnumerator {
                index: AtomicU32::new(self.index.load(Ordering::Acquire)),
                _module: ModuleLease::new(),
            }
            .into())
        })
    }

    /// 按 COM 枚举约定返回至多一个属性；不足请求数量时以 `S_FALSE` 表示。
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

            // 此枚举器只有一个输入属性；部分满足或已耗尽均以 S_FALSE 返回。
            Err(Error::from_hresult(S_FALSE))
        })
    }

    /// 将游标恢复到唯一属性之前。
    fn Reset(&self) -> Result<()> {
        boundary::guard(None, || {
            self.index.store(0, Ordering::Release);
            Ok(())
        })
    }

    /// 跳过任意正数项都会耗尽这个单项枚举器。
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
    /// 向 TSF 注册显示属性 GUID；注册失败只记录诊断，不阻断服务激活。
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

    /// 在有效编辑会话中把已注册属性写入指定范围。
    ///
    /// `ec` 必须由当前 TSF 编辑会话提供；TSF 属性调用失败会记录并原样返回错误。
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

    /// 清除范围上的显示属性，调用方必须传入当前编辑会话的 cookie。
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

    /// 尽力设置显示属性；显示效果失败不会改变文本编辑流程的结果。
    pub(super) fn set_display_attribute_best_effort(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) {
        let _ = self.set_display_attribute(context, ec, range);
    }

    /// 尽力清除显示属性；清理失败由底层记录，不向调用方传播。
    pub(super) fn clear_display_attribute_best_effort(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
    ) {
        let _ = self.clear_display_attribute(context, ec, range);
    }
}
