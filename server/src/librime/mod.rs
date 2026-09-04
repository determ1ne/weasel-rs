mod api;
mod loader;
mod raw;

use std::{
    ffi::{CStr, CString},
    mem::size_of,
    path::Path,
    rc::Rc,
};

use weasel_common::message::{
    Candidate, KeyEvent, KeyEventResponse, RendererEvent, RendererEventAction,
};

use self::loader::RimeLibrary;

const RIME_TRUE: std::os::raw::c_int = 1;
const MAX_CANDIDATES: usize = 256;

// Construct only after a successful getter. The matching free callback has
// already been checked before invoking that getter, including in test tables.
struct Output<T> {
    value: T,
    free: unsafe extern "C" fn(*mut T) -> i32,
}

impl<T> std::ops::Deref for Output<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> Drop for Output<T> {
    fn drop(&mut self) {
        unsafe {
            (self.free)(&mut self.value);
        }
    }
}

fn candidate_count(count: i32) -> Option<usize> {
    usize::try_from(count)
        .ok()
        .filter(|&count| count <= MAX_CANDIDATES)
}

fn user_data_directory(base_dir: &Path) -> Result<CString, String> {
    let directory = weasel_common::runtime_paths::ensure_user_data_directory(base_dir)
        .map_err(|error| format!("could not prepare user data directory: {error}"))?;
    CString::new(directory.to_string_lossy().as_bytes())
        .map_err(|error| format!("invalid user data directory path: {error}"))
}

pub struct Librime {
    inner: Rc<Engine>,
}

struct Engine {
    api: RimeLibraryApi,
    // None is used only by fake-table tests; production engines always own the DLL.
    _library: Option<RimeLibrary>,
    app_name: CString,
    shared_data_dir: CString,
    user_data_dir: CString,
}

pub struct RimeSession {
    api: RimeLibraryApi,
    id: raw::RimeSessionId,
    // Keeps finalize and DLL unload after destroy_session, even if Librime is dropped first.
    _engine: Rc<Engine>,
}

#[derive(Clone)]
struct RimeLibraryApi {
    api: Rc<raw::RimeApi>,
}

impl Librime {
    pub fn load(base_dir: &Path) -> Result<Self, String> {
        let library = RimeLibrary::load(&base_dir.join("x64").join("rime.dll"))?;
        let api = unsafe { RimeLibraryApi::load(library.api().as_ptr(), false)? };
        let app_name = CString::new("rime.weasel-rs").expect("static app name has no NUL");
        let shared_data_dir = CString::new(base_dir.join("rime-data").to_string_lossy().as_bytes())
            .map_err(|error| format!("invalid shared data directory path: {error}"))?;
        let user_data_dir = user_data_directory(base_dir)?;
        let mut traits: raw::RimeTraits = unsafe { std::mem::zeroed() };
        traits.data_size = struct_data_size::<raw::RimeTraits>();
        traits.shared_data_dir = shared_data_dir.as_ptr().cast();
        traits.user_data_dir = user_data_dir.as_ptr().cast();
        traits.app_name = app_name.as_ptr().cast();

        unsafe {
            api.required("setup", (*api.api).setup)?(&mut traits);
            api.required("initialize", (*api.api).initialize)?(&mut traits);
        }

        Ok(Self {
            inner: Rc::new(Engine {
                api,
                _library: Some(library),
                app_name,
                shared_data_dir,
                user_data_dir,
            }),
        })
    }

    pub fn deploy(base_dir: &Path) -> Result<(), String> {
        let library = RimeLibrary::load(&base_dir.join("x64").join("rime.dll"))?;
        let api = unsafe { RimeLibraryApi::load(library.api().as_ptr(), true)? };
        let app_name = CString::new("rime.weasel-rs").expect("static app name has no NUL");
        let shared_data_dir = CString::new(base_dir.join("rime-data").to_string_lossy().as_bytes())
            .map_err(|error| format!("invalid shared data directory path: {error}"))?;
        let user_data_dir = user_data_directory(base_dir)?;
        let log_dir = CString::new("").expect("empty string has no NUL");
        let mut traits: raw::RimeTraits = unsafe { std::mem::zeroed() };
        traits.data_size = struct_data_size::<raw::RimeTraits>();
        traits.shared_data_dir = shared_data_dir.as_ptr().cast();
        traits.user_data_dir = user_data_dir.as_ptr().cast();
        traits.app_name = app_name.as_ptr().cast();
        traits.log_dir = log_dir.as_ptr().cast();

        unsafe {
            api.required("setup", (*api.api).setup)?(&mut traits);
            api.required("deployer_initialize", (*api.api).deployer_initialize)?(&mut traits);
            let deployed = api.required("deploy", (*api.api).deploy)?() == RIME_TRUE;
            api.required("finalize", (*api.api).finalize)?();
            if deployed {
                Ok(())
            } else {
                Err("librime deploy returned failure".to_owned())
            }
        }
    }

    pub fn new_session(&self) -> Result<RimeSession, String> {
        let api = self.inner.api.clone();
        let id = unsafe { api.required("create_session", (*api.api).create_session)?() };
        if id == 0 {
            return Err("librime create_session returned zero".to_owned());
        }
        Ok(RimeSession {
            api,
            id,
            _engine: self.inner.clone(),
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let api = &self.api;
        unsafe {
            if let Ok(finalize) = api.required("finalize", (*api.api).finalize) {
                finalize();
            }
        }
        let _ = &self.app_name;
        let _ = &self.shared_data_dir;
        let _ = &self.user_data_dir;
    }
}

impl RimeSession {
    pub fn context_action(
        &mut self,
        action: weasel_common::message::ContextAction,
    ) -> KeyEventResponse {
        use weasel_common::message::ContextAction;
        unsafe {
            match action {
                ContextAction::ToggleAscii => {
                    if let (Some(get), Some(set)) =
                        ((*self.api.api).get_option, (*self.api.api).set_option)
                    {
                        // Finish existing preedit before switching modes so the
                        // TIP receives its commit on the same ordered reply stream.
                        if let Some(commit) = (*self.api.api).commit_composition {
                            commit(self.id);
                        }
                        let ascii = get(self.id, c"ascii_mode".as_ptr()) != 0;
                        set(self.id, c"ascii_mode".as_ptr(), (!ascii) as i32);
                    }
                    return self.read_response(false, String::new());
                }
                ContextAction::Focus => {
                    let mut response = KeyEventResponse::default();
                    if let Some(get) = (*self.api.api).get_option {
                        response.ascii_mode = Some(get(self.id, c"ascii_mode".as_ptr()) != 0);
                    }
                    return response;
                }
                ContextAction::Submit => {
                    if let Some(commit) = (*self.api.api).commit_composition {
                        commit(self.id);
                    }
                    return self.read_response(false, String::new());
                }
                ContextAction::Cancel | ContextAction::HostTerminated => {
                    if let Some(clear) = (*self.api.api).clear_composition {
                        clear(self.id);
                    }
                    // Drain any engine commit without reinserting host-owned text.
                    let mut response = self.read_response(false, String::new());
                    response.commit_text.clear();
                    return response;
                }
                _ => KeyEventResponse::default(),
            }
        }
    }

    pub fn process_key(&mut self, event: &KeyEvent) -> KeyEventResponse {
        if event.test {
            return KeyEventResponse::default();
        }

        let Some(keycode) = event.keycode else {
            return KeyEventResponse::default();
        };
        let mask = event.modifiers;
        let eaten = unsafe {
            self.api
                .required("process_key", (*self.api.api).process_key)
                .and_then(|process| Ok(process(self.id, keycode, mask) == RIME_TRUE))
                .unwrap_or(false)
        };
        self.read_response(eaten, String::new())
    }

    pub fn process_renderer_event(&mut self, event: &RendererEvent) -> KeyEventResponse {
        let result = unsafe {
            match RendererEventAction::try_from(event.action).ok() {
                Some(RendererEventAction::ItemInvoked)
                    if (event.item_index as u64) >= MAX_CANDIDATES as u64 =>
                {
                    Ok(false)
                }
                Some(RendererEventAction::ItemInvoked) => self
                    .api
                    .required(
                        "select_candidate_on_current_page",
                        (*self.api.api).select_candidate_on_current_page,
                    )
                    .map(|select| select(self.id, event.item_index as usize) == RIME_TRUE),
                Some(RendererEventAction::NavigatePrevious) => self
                    .api
                    .required("change_page", (*self.api.api).change_page)
                    .map(|change| change(self.id, RIME_TRUE) == RIME_TRUE),
                Some(RendererEventAction::NavigateNext) => self
                    .api
                    .required("change_page", (*self.api.api).change_page)
                    .map(|change| change(self.id, 0) == RIME_TRUE),
                Some(RendererEventAction::Dismiss) => self
                    .api
                    .required("clear_composition", (*self.api.api).clear_composition)
                    .map(|clear| {
                        clear(self.id);
                        true
                    }),
                Some(RendererEventAction::OpenEmojiPanel) => {
                    let Some(clear) = (*self.api.api).clear_composition else {
                        eprintln!(
                            "weasel-server: cannot open emoji panel: clear_composition unavailable"
                        );
                        return KeyEventResponse::default();
                    };
                    clear(self.id);
                    return KeyEventResponse {
                        state_updated: true,
                        open_emoji_panel: true,
                        ..Default::default()
                    };
                }
                None | Some(RendererEventAction::Unspecified) => Ok(false),
            }
        };
        self.read_response(result.unwrap_or(false), String::new())
    }

    fn read_response(&mut self, eaten: bool, mut commit_text: String) -> KeyEventResponse {
        unsafe {
            if let (Some(get_commit), Some(free)) =
                (self.api.api.get_commit, self.api.api.free_commit)
            {
                let mut commit: raw::RimeCommit = std::mem::zeroed();
                commit.data_size = struct_data_size::<raw::RimeCommit>();
                if get_commit(self.id, &mut commit) == RIME_TRUE {
                    let commit = Output {
                        value: commit,
                        free,
                    };
                    if !commit.text.is_null() {
                        commit_text = c_string(commit.text);
                    }
                }
            }

            let mut response = KeyEventResponse {
                eaten,
                commit_text,
                ..Default::default()
            };

            if let (Some(get_context), Some(free)) =
                (self.api.api.get_context, self.api.api.free_context)
            {
                let mut context: raw::RimeContext = std::mem::zeroed();
                context.data_size = struct_data_size::<raw::RimeContext>();
                if get_context(self.id, &mut context) == RIME_TRUE {
                    let context = Output {
                        value: context,
                        free,
                    };
                    response.state_updated = true;
                    response.page_start = (context.menu.page_no.max(0) as u32)
                        .saturating_mul(context.menu.page_size.max(0) as u32);
                    response.can_page_previous = context.menu.page_no > 0;
                    response.can_page_next =
                        context.menu.is_last_page == 0 && context.menu.num_candidates > 0;
                    response.composing = context.composition.length > 0;
                    if !context.composition.preedit.is_null() {
                        response.composition = c_string(context.composition.preedit);
                    }
                    let mut cursor = (context.composition.cursor_pos.max(0) as usize)
                        .min(response.composition.len());
                    while !response.composition.is_char_boundary(cursor) {
                        cursor -= 1;
                    }
                    response.composition_cursor =
                        response.composition[..cursor].encode_utf16().count() as u32;
                    if let Some(count) = candidate_count(context.menu.num_candidates)
                        .filter(|&count| count > 0 && !context.menu.candidates.is_null())
                    {
                        let candidates = std::slice::from_raw_parts(context.menu.candidates, count);
                        response.candidates = candidates
                            .iter()
                            .map(|candidate| Candidate {
                                text: c_string(candidate.text),
                                comment: c_string(candidate.comment),
                            })
                            .collect();
                    }
                    if context.menu.highlighted_candidate_index >= 0
                        && (context.menu.highlighted_candidate_index as usize)
                            < response.candidates.len()
                    {
                        response.selected_candidate =
                            context.menu.highlighted_candidate_index as u32;
                    }
                }
            }
            if let (Some(get_status), Some(free_status)) =
                ((*self.api.api).get_status, (*self.api.api).free_status)
            {
                let mut status: raw::RimeStatus = std::mem::zeroed();
                status.data_size = struct_data_size::<raw::RimeStatus>();
                if get_status(self.id, &mut status) == RIME_TRUE {
                    let status = Output {
                        value: status,
                        free: free_status,
                    };
                    response.composing = status.is_composing != 0;
                    response.ascii_mode = Some(status.is_ascii_mode != 0);
                }
            }
            response
        }
    }
}

impl Drop for RimeSession {
    fn drop(&mut self) {
        unsafe {
            if let Ok(destroy) = self
                .api
                .required("destroy_session", (*self.api.api).destroy_session)
            {
                let _ = destroy(self.id);
            }
        }
    }
}

impl RimeLibraryApi {
    unsafe fn required<T>(&self, name: &str, function: Option<T>) -> Result<T, String> {
        function.ok_or_else(|| format!("librime API function is unavailable: {name}"))
    }
}

#[cfg(test)]
mod input_mode_tests {
    use super::*;
    use std::cell::RefCell;
    use weasel_common::message::ContextAction;

    thread_local! {
        static STATE: RefCell<(bool, Vec<&'static str>)> = const { RefCell::new((false, Vec::new())) };
        static FREES: RefCell<[usize; 3]> = const { RefCell::new([0; 3]) };
    }

    unsafe extern "C" fn create_session() -> raw::RimeSessionId {
        1
    }
    unsafe extern "C" fn destroy_session(_: raw::RimeSessionId) -> i32 {
        STATE.with(|state| state.borrow_mut().1.push("destroy"));
        1
    }
    unsafe extern "C" fn finalize() {
        STATE.with(|state| state.borrow_mut().1.push("finalize"));
    }

    fn fake_engine(mut table: raw::RimeApi) -> Librime {
        table.create_session = Some(create_session);
        table.destroy_session = Some(destroy_session);
        table.finalize = Some(finalize);
        Librime {
            inner: Rc::new(Engine {
                api: RimeLibraryApi {
                    api: Rc::new(table),
                },
                _library: None,
                app_name: CString::default(),
                shared_data_dir: CString::default(),
                user_data_dir: CString::default(),
            }),
        }
    }

    #[test]
    fn sessions_keep_engine_alive_until_last_destroy() {
        STATE.with(|state| *state.borrow_mut() = (false, Vec::new()));
        let engine = fake_engine(unsafe { std::mem::zeroed() });
        let first = engine.new_session().unwrap();
        let second = engine.new_session().unwrap();
        drop(engine);
        STATE.with(|state| assert!(state.borrow().1.is_empty()));
        drop(first);
        STATE.with(|state| assert_eq!(state.borrow().1, ["destroy"]));
        drop(second);
        STATE.with(|state| assert_eq!(state.borrow().1, ["destroy", "destroy", "finalize"]));
    }

    unsafe extern "C" fn get_commit(_: raw::RimeSessionId, output: *mut raw::RimeCommit) -> i32 {
        unsafe {
            (*output).text = c"committed".as_ptr().cast_mut();
        }
        1
    }
    unsafe extern "C" fn free_commit(_: *mut raw::RimeCommit) -> i32 {
        FREES.with(|frees| frees.borrow_mut()[0] += 1);
        1
    }
    unsafe extern "C" fn invalid_context(
        _: raw::RimeSessionId,
        output: *mut raw::RimeContext,
    ) -> i32 {
        unsafe {
            (*output).menu.num_candidates = i32::MAX;
            (*output).menu.candidates = std::ptr::dangling_mut();
        }
        1
    }
    unsafe extern "C" fn free_context(_: *mut raw::RimeContext) -> i32 {
        FREES.with(|frees| frees.borrow_mut()[1] += 1);
        1
    }

    #[test]
    fn malformed_count_is_rejected_and_all_successful_outputs_are_freed() {
        FREES.with(|frees| *frees.borrow_mut() = [0; 3]);
        let mut table: raw::RimeApi = unsafe { std::mem::zeroed() };
        table.get_commit = Some(get_commit);
        table.free_commit = Some(free_commit);
        table.get_context = Some(invalid_context);
        table.free_context = Some(free_context);
        table.get_status = Some(get_status);
        table.free_status = Some(free_status);
        let mut session = fake_engine(table).new_session().unwrap();
        let response = session.read_response(false, String::new());
        assert_eq!(response.commit_text, "committed");
        assert!(response.candidates.is_empty());
        FREES.with(|frees| assert_eq!(*frees.borrow(), [1, 1, 1]));
    }

    #[test]
    fn output_cleanup_runs_during_unwind() {
        FREES.with(|frees| *frees.borrow_mut() = [0; 3]);
        let result = std::panic::catch_unwind(|| {
            let _commit = Output {
                value: unsafe { std::mem::zeroed() },
                free: free_commit,
            };
            let _context = Output {
                value: unsafe { std::mem::zeroed() },
                free: free_context,
            };
            let _status = Output {
                value: unsafe { std::mem::zeroed() },
                free: free_status,
            };
            panic!("conversion failed");
        });
        assert!(result.is_err());
        FREES.with(|frees| assert_eq!(*frees.borrow(), [1, 1, 1]));
    }

    #[test]
    fn failed_getters_do_not_free_unowned_outputs() {
        unsafe extern "C" fn no_commit(_: raw::RimeSessionId, _: *mut raw::RimeCommit) -> i32 {
            0
        }
        unsafe extern "C" fn no_context(_: raw::RimeSessionId, _: *mut raw::RimeContext) -> i32 {
            0
        }
        unsafe extern "C" fn no_status(_: raw::RimeSessionId, _: *mut raw::RimeStatus) -> i32 {
            0
        }
        FREES.with(|frees| *frees.borrow_mut() = [0; 3]);
        let mut table: raw::RimeApi = unsafe { std::mem::zeroed() };
        table.get_commit = Some(no_commit);
        table.free_commit = Some(free_commit);
        table.get_context = Some(no_context);
        table.free_context = Some(free_context);
        table.get_status = Some(no_status);
        table.free_status = Some(free_status);
        let mut session = fake_engine(table).new_session().unwrap();
        assert!(!session.read_response(false, String::new()).state_updated);
        FREES.with(|frees| assert_eq!(*frees.borrow(), [0; 3]));
    }

    #[test]
    fn candidate_count_has_explicit_bounds() {
        assert_eq!(candidate_count(-1), None);
        assert_eq!(candidate_count(0), Some(0));
        assert_eq!(candidate_count(MAX_CANDIDATES as i32), Some(MAX_CANDIDATES));
        assert_eq!(candidate_count(MAX_CANDIDATES as i32 + 1), None);
    }

    #[test]
    fn untranslated_and_test_keys_never_enter_librime() {
        STATE.with(|state| *state.borrow_mut() = (false, Vec::new()));
        unsafe extern "C" fn process(_: raw::RimeSessionId, _: i32, _: i32) -> i32 {
            STATE.with(|state| state.borrow_mut().1.push("key"));
            1
        }
        let mut table: raw::RimeApi = unsafe { std::mem::zeroed() };
        table.process_key = Some(process);
        let mut session = fake_engine(table).new_session().unwrap();
        assert!(
            !session
                .process_key(&KeyEvent {
                    virtual_key: 0x41,
                    ..Default::default()
                })
                .eaten
        );
        assert!(
            !session
                .process_key(&KeyEvent {
                    keycode: Some(97),
                    test: true,
                    ..Default::default()
                })
                .eaten
        );
        STATE.with(|state| assert!(state.borrow().1.is_empty()));
        assert!(
            session
                .process_key(&KeyEvent {
                    keycode: Some(97),
                    ..Default::default()
                })
                .eaten
        );
        STATE.with(|state| assert_eq!(state.borrow().1, ["key"]));
    }

    unsafe extern "C" fn get_option(_: raw::RimeSessionId, _: *const std::ffi::c_char) -> i32 {
        STATE.with(|state| state.borrow().0 as i32)
    }

    unsafe extern "C" fn set_option(_: raw::RimeSessionId, _: *const std::ffi::c_char, value: i32) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.0 = value != 0;
            state.1.push("set");
        });
    }

    unsafe extern "C" fn commit(_: raw::RimeSessionId) -> i32 {
        STATE.with(|state| state.borrow_mut().1.push("commit"));
        1
    }

    unsafe extern "C" fn get_status(_: raw::RimeSessionId, status: *mut raw::RimeStatus) -> i32 {
        unsafe {
            (*status).is_ascii_mode = STATE.with(|state| state.borrow().0 as i32);
        }
        1
    }

    unsafe extern "C" fn free_status(_: *mut raw::RimeStatus) -> i32 {
        FREES.with(|frees| frees.borrow_mut()[2] += 1);
        1
    }

    #[test]
    fn toggle_finishes_preedit_before_switch_and_reports_engine_mode() {
        STATE.with(|state| *state.borrow_mut() = (false, Vec::new()));
        // Fake function table only: never load rime.dll or touch user data.
        let mut api: raw::RimeApi = unsafe { std::mem::zeroed() };
        api.get_option = Some(get_option);
        api.set_option = Some(set_option);
        api.commit_composition = Some(commit);
        api.get_status = Some(get_status);
        api.free_status = Some(free_status);
        let mut session = fake_engine(api).new_session().unwrap();
        let focus = session.context_action(ContextAction::Focus);
        assert_eq!(focus.ascii_mode, Some(false));
        assert!(!focus.state_updated);
        assert!(focus.commit_text.is_empty());
        assert_eq!(
            session
                .context_action(ContextAction::ToggleAscii)
                .ascii_mode,
            Some(true)
        );
        assert_eq!(
            session
                .context_action(ContextAction::ToggleAscii)
                .ascii_mode,
            Some(false)
        );
        STATE.with(|state| assert_eq!(state.borrow().1, ["commit", "set", "commit", "set"]));
    }
}

fn struct_data_size<T>() -> i32 {
    (size_of::<T>() - size_of::<i32>()) as i32
}

// The engine must provide a readable NUL-terminated string for the lifetime of
// its output guard. Raw pointer validity cannot be established by Rust.
unsafe fn c_string(pointer: *const std::os::raw::c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer).to_string_lossy().into_owned() }
}
