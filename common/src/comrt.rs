//! Windows COM 与 WinRT apartment 的线程局部生命周期守卫。

use std::{marker::PhantomData, rc::Rc};

use crate::bindings::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, RO_INIT_SINGLETHREADED, RoInitialize,
    RoUninitialize,
};

/// 当前线程的一次 COM STA 初始化计数。
pub struct ComApartment(PhantomData<Rc<()>>);

impl ComApartment {
    /// 将当前线程初始化为 COM STA。
    pub fn initialize_sta() -> windows_core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as u32).ok()? };
        Ok(Self(PhantomData))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// 当前线程的一次 WinRT STA 初始化计数。
pub struct WinRtApartment(PhantomData<Rc<()>>);

impl WinRtApartment {
    /// 将当前线程初始化为 WinRT STA。
    pub fn initialize_sta() -> windows_core::Result<Self> {
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED).ok()? };
        Ok(Self(PhantomData))
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}
