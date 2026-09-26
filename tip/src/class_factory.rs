//! TSF 文本服务的 COM 类工厂。
//!
//! 工厂只负责校验聚合请求并创建新的 [`TextService`] 实例；[`ModuleLease`] 将 DLL
//! 生命周期与工厂及其创建的对象绑定，避免 COM 仍持有对象时模块被卸载。

use crate::{
    bindings::*,
    boundary,
    module::{self, ModuleLease},
    registration,
    service::TextService,
};
use std::{ffi::c_void, ptr};
use windows_core::{BOOL, Error, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

#[implement(IClassFactory)]
struct ClassFactory {
    _module: ModuleLease,
}

impl IClassFactory_Impl for ClassFactory_Impl {
    /// 创建非聚合的文本服务对象，并按 COM 请求查询接口。
    ///
    /// 这是 COM ABI 边界：先验证并清空输出指针，再拒绝聚合；HRESULT 错误经 `Result`
    /// 返回给生成的 COM 适配层。整个操作由边界保护器包裹，panic 不穿越 COM。
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        boundary::guard(None, || {
            if ppvobject.is_null() || riid.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }

            unsafe {
                *ppvobject = ptr::null_mut();
            }
            if !punkouter.is_null() {
                return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
            }

            let object: IUnknown = TextService::new().into();
            let hr = unsafe { object.query(riid, ppvobject) };
            if hr.is_ok() {
                Ok(())
            } else {
                Err(Error::from_hresult(hr))
            }
        })
    }

    /// 增减服务器锁计数，使 COM 宿主显式锁定期间 DLL 保持加载。
    fn LockServer(&self, flock: BOOL) -> Result<()> {
        boundary::guard(None, || {
            module::lock_server(flock.as_bool());
            Ok(())
        })
    }
}

/// 返回 Weasel-RS TSF 类的 COM 类工厂。
///
/// 此导出函数遵循系统 COM ABI：无效指针返回 `E_POINTER`，未知 CLSID 返回
/// `CLASS_E_CLASSNOTAVAILABLE`，请求接口不受支持时返回 `E_NOINTERFACE`。返回的工厂持有
/// 模块租约，确保 COM 尚在使用它时 DLL 不会被卸载。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    boundary::guard(None, || {
        Ok((|| unsafe {
            if rclsid.is_null() || riid.is_null() || ppv.is_null() {
                return E_POINTER;
            }
            *ppv = ptr::null_mut();
            if *rclsid != CLSID_WEASEL_TIP {
                return CLASS_E_CLASSNOTAVAILABLE;
            }

            let factory: IClassFactory = ClassFactory {
                _module: ModuleLease::new(),
            }
            .into();
            let hr = factory.query(riid, ppv);
            if hr.is_ok() { S_OK } else { E_NOINTERFACE }
        })())
    })
    .unwrap_or_else(|error| error.code())
}

/// 当 COM 对象、工厂或服务器锁仍存活时，告知 COM 不要卸载此 DLL。
///
/// 结果映射到 `S_OK`/`S_FALSE`；检查时顺带回收已结束的诊断线程记录，不等待仍在运行的线程。
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if module::can_unload() { S_OK } else { S_FALSE }
}

#[unsafe(no_mangle)]
/// 注册 COM 服务器及 TSF 配置文件和类别，并将 Rust 错误转换为 HRESULT。
pub extern "system" fn DllRegisterServer() -> HRESULT {
    boundary::guard(None, || {
        Ok((|| match registration::register() {
            Ok(()) => S_OK,
            Err(error) => error.code(),
        })())
    })
    .unwrap_or_else(|error| error.code())
}

#[unsafe(no_mangle)]
/// 撤销本 TIP 的 TSF 类别、配置文件和 COM 注册。
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    boundary::guard(None, || {
        Ok((|| {
            registration::unregister();
            S_OK
        })())
    })
    .unwrap_or_else(|error| error.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_alone_keeps_dll_loaded() {
        let mut raw = ptr::null_mut();
        unsafe {
            assert_eq!(
                DllGetClassObject(&CLSID_WEASEL_TIP, &IClassFactory::IID, &mut raw),
                S_OK
            );
            let factory = IClassFactory::from_raw(raw);
            assert_eq!(DllCanUnloadNow(), S_FALSE);
            factory.LockServer(true).unwrap();
            factory.LockServer(false).unwrap();
            assert_eq!(DllCanUnloadNow(), S_FALSE);
        }
    }
}
