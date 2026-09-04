use super::{RimeLibraryApi, raw};
use std::{
    mem::{offset_of, size_of},
    rc::Rc,
};

impl RimeLibraryApi {
    /// `source` must point to the DLL's readable, immutable versioned API table.
    /// RimeApi uses data_size as its ABI version; get_version is informational.
    pub(super) unsafe fn load(source: *const raw::RimeApi, deploy: bool) -> Result<Self, String> {
        if source.is_null() {
            return Err("librime returned a null API table".into());
        }
        // Read only the mandatory header before considering any function field.
        let data_size = unsafe { source.cast::<i32>().read() };
        let bytes = usize::try_from(data_size)
            .ok()
            .and_then(|size| size.checked_add(size_of::<i32>()))
            .ok_or_else(|| "invalid librime API data_size".to_owned())?;
        let mut snapshot: raw::RimeApi = unsafe { std::mem::zeroed() };
        macro_rules! field {
            ($name:ident) => {{
                // Never form a reference to the entire foreign table: older DLLs
                // may allocate only a prefix of the current header's structure.
                let end = offset_of!(raw::RimeApi, $name) + size_of_val(&snapshot.$name);
                if bytes >= end {
                    snapshot.$name = unsafe { std::ptr::addr_of!((*source).$name).read() };
                }
            }};
        }
        field!(setup);
        field!(initialize);
        field!(finalize);
        field!(deployer_initialize);
        field!(deploy);
        field!(create_session);
        field!(destroy_session);
        field!(process_key);
        field!(commit_composition);
        field!(clear_composition);
        field!(get_commit);
        field!(free_commit);
        field!(get_context);
        field!(free_context);
        field!(get_status);
        field!(free_status);
        field!(get_option);
        field!(set_option);
        field!(select_candidate_on_current_page);
        field!(change_page);
        macro_rules! required {
            ($($name:ident),+ $(,)?) => { $(
                if snapshot.$name.is_none() {
                    return Err(format!("librime API function is unavailable or outside data_size: {}", stringify!($name)));
                }
            )+ };
        }
        required!(setup, finalize);
        if deploy {
            required!(deployer_initialize, deploy);
        } else {
            required!(
                initialize,
                create_session,
                destroy_session,
                process_key,
                commit_composition,
                clear_composition,
                get_commit,
                free_commit,
                get_context,
                free_context,
                get_status,
                free_status,
                get_option,
                set_option
            );
        }
        // Candidate selection and change_page remain optional for older engines.
        Ok(Self {
            api: Rc::new(snapshot),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn setup(_: *mut raw::RimeTraits) {}
    unsafe extern "C" fn finalize() {}
    unsafe extern "C" fn deploy() -> i32 {
        1
    }
    unsafe extern "C" fn change_page(_: raw::RimeSessionId, _: i32) -> i32 {
        1
    }

    fn deploy_table() -> raw::RimeApi {
        let mut table: raw::RimeApi = unsafe { std::mem::zeroed() };
        table.data_size = super::super::struct_data_size::<raw::RimeApi>();
        table.setup = Some(setup);
        table.deployer_initialize = Some(setup);
        table.finalize = Some(finalize);
        table.deploy = Some(deploy);
        table.change_page = Some(change_page);
        table
    }

    #[test]
    fn older_table_does_not_expose_fields_beyond_its_version() {
        let mut table = deploy_table();
        table.data_size = (offset_of!(raw::RimeApi, change_page) - size_of::<i32>()) as i32;
        let api = unsafe { RimeLibraryApi::load(&table, true) }.unwrap();
        assert!(api.api.deploy.is_some());
        assert!(api.api.change_page.is_none());
    }

    #[test]
    fn future_table_versions_are_accepted() {
        let mut table = deploy_table();
        table.data_size += 128;
        let api = unsafe { RimeLibraryApi::load(&table, true) }.unwrap();
        assert!(api.api.change_page.is_some());
    }

    #[test]
    fn missing_finalize_or_initialize_is_rejected_upfront() {
        let mut table = deploy_table();
        table.finalize = None;
        let error = unsafe { RimeLibraryApi::load(&table, true) }.err().unwrap();
        assert!(error.contains("finalize"));
        table.finalize = Some(finalize);
        let error = unsafe { RimeLibraryApi::load(&table, false) }
            .err()
            .unwrap();
        assert!(error.contains("initialize"));
    }

    #[test]
    fn header_only_and_negative_sizes_are_rejected_without_reading_fields() {
        for size in [-1i32, 0, 1] {
            assert!(unsafe { RimeLibraryApi::load((&size as *const i32).cast(), false) }.is_err());
        }
    }

    #[test]
    fn partial_function_pointer_is_not_available() {
        let mut table: raw::RimeApi = unsafe { std::mem::zeroed() };
        table.data_size = (offset_of!(raw::RimeApi, setup) + size_of_val(&table.setup)
            - size_of::<i32>()
            - 1) as i32;
        let error = unsafe { RimeLibraryApi::load(&table, false) }
            .err()
            .unwrap();
        assert!(error.contains("setup"));
    }
}
