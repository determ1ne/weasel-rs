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

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        boundary::guard(None, || {
            module::lock_server(flock.as_bool());
            Ok(())
        })
    }
}

/// Return the class factory for the weasel-rs TSF class.
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

/// Keep the DLL loaded while a COM object or class-factory lock is active.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if module::can_unload() { S_OK } else { S_FALSE }
}

#[unsafe(no_mangle)]
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
