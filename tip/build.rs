use std::path::PathBuf;
#[path = "../build_support/version.rs"]
mod version;

fn main() {
    version::embed();
    embed_icons();
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let manual_rdl = manifest_dir.join("metadata/text_services_manual.rdl");
    let weasel_rdl = manifest_dir.join("metadata/weasel.rdl");
    let module_definition = manifest_dir.join("weasel-tip.def");
    let extra_winmd = out.join("tsf-extra.winmd");
    let weasel_extra_winmd = out.join("weasel-extra.winmd");
    let bindings = out.join("bindings.rs");

    windows_rdl::reader()
        .input(&manual_rdl)
        .reference_default()
        .output(&extra_winmd)
        .write()
        .expect("failed to compile TextServices manual RDL");

    windows_rdl::reader()
        .input(&weasel_rdl)
        .reference_default()
        .output(&weasel_extra_winmd)
        .write()
        .expect("failed to compile weasel-rs manual RDL");

    windows_bindgen::builder()
        .input(&extra_winmd)
        .input(&weasel_extra_winmd)
        .input_default()
        .output(&bindings)
        .filters([
            "Windows.Win32.E_PENDING",
            "Windows.Win32.SetTimer",
            "Windows.Win32.KillTimer",
            "Windows.Win32.WM_TIMER",
            "Windows.Win32.OutputDebugStringW",
            "Windows.Win32.WaitForSingleObject",
            "Windows.Win32.WAIT_OBJECT_0",
            "Windows.Win32.FindWindowW",
            "Windows.Win32.WM_COMMAND",
            "Windows.Win32.CreatePopupMenu",
            "Windows.Win32.DestroyMenu",
            "Windows.Win32.AppendMenuW",
            "Windows.Win32.TrackPopupMenu",
            "Windows.Win32.GetFocus",
            "Windows.Win32.TPM_NONOTIFY",
            "Windows.Win32.TPM_RETURNCMD",
            "Windows.Win32.TPM_RIGHTBUTTON",
            "Windows.Win32.MF_STRING",
            "Windows.Win32.MF_SEPARATOR",
            "Windows.Win32.WM_SETTINGCHANGE",
            "Windows.Win32.WM_THEMECHANGED",
            "Windows.Win32.WM_SYSCOLORCHANGE",
            "Windows.Win32.WS_POPUP",
            "Windows.Win32.WS_EX_TOOLWINDOW",
            "Windows.Win32.WS_EX_NOACTIVATE",
            "Windows.Win32.UI.TextServices.GUID_LBI_INPUTMODE",
            "Windows.Win32.ITfLangBarItemMgr",
            "Windows.Win32.ITfLangBarItemButton",
            "Windows.Win32.ITfLangBarItem",
            "Windows.Win32.ITfMenu",
            "Windows.Win32.TF_LANGBARITEMINFO",
            "Windows.Win32.TfLBIClick",
            "Windows.Win32.ITfLangBarItemSink",
            "Windows.Win32.TF_LBI_STYLE_BTN_BUTTON",
            "Windows.Win32.TF_LBI_STYLE_SHOWNINTRAY",
            "Windows.Win32.TF_LBI_ICON",
            "Windows.Win32.TF_LBI_TEXT",
            "Windows.Win32.TF_LBI_TOOLTIP",
            "Windows.Win32.TF_LBI_STATUS",
            "Windows.Win32.TF_LBI_STATUS_DISABLED",
            "Windows.Win32.CONNECT_E_CANNOTCONNECT",
            "Windows.Win32.CONNECT_E_ADVISELIMIT",
            "Windows.Win32.CONNECT_E_NOCONNECTION",
            "Windows.Win32.E_INVALIDARG",
            "Windows.Win32.LoadImageW",
            "Windows.Win32.IMAGE_ICON",
            "Windows.Win32.GetSystemMetrics",
            "Windows.Win32.SM_CXSMICON",
            "Windows.Win32.SM_CYSMICON",
            "Windows.Win32.GetSysColor",
            "Windows.Win32.COLOR_WINDOWTEXT",
            "Windows.Win32.GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT",
            "Windows.Win32.RegisterClassW",
            "Windows.Win32.UnregisterClassW",
            "Windows.Win32.CreateWindowExW",
            "Windows.Win32.DestroyWindow",
            "Windows.Win32.DefWindowProcW",
            "Windows.Win32.GetWindowLongPtrW",
            "Windows.Win32.SetWindowLongPtrW",
            "Windows.Win32.GetModuleHandleW",
            "Windows.Win32.PostMessageW",
            "Windows.Win32.HWND_MESSAGE",
            "Windows.Win32.GWLP_USERDATA",
            "Windows.Win32.WM_APP",
            "Windows.Win32.SendInput",
            "Windows.Win32.GetMessageExtraInfo",
            "Windows.Win32.INPUT_KEYBOARD",
            "Windows.Win32.KEYEVENTF_KEYUP",
            "Windows.Win32.VK_LWIN",
            "Windows.Win32.VK_OEM_PERIOD",
            "Windows.Win32.GetKeyboardState",
            "Windows.Win32.GetMessageTime",
            "Windows.Win32.GetKeyboardLayout",
            "Windows.Win32.ToUnicodeEx",
            "Weasel.CLSID_WEASEL_TIP",
            "Windows.Win32.IClassFactory",
            "Windows.Win32.ITfTextInputProcessor",
            "Windows.Win32.ITfTextInputProcessorEx",
            "Windows.Win32.ITfSource",
            "Windows.Win32.ITfThreadMgr",
            "Windows.Win32.ITfThreadMgrEventSink",
            "Windows.Win32.ITfDocumentMgr",
            "Windows.Win32.ITfContext",
            "Windows.Win32.ITfContextView",
            "Windows.Win32.ITfContextComposition",
            "Windows.Win32.ITfInsertAtSelection",
            "Windows.Win32.ITfComposition",
            "Windows.Win32.ITfRange",
            "Windows.Win32.ITfEditRecord",
            "Windows.Win32.TF_DEFAULT_SELECTION",
            "Windows.Win32.ITfProperty",
            "Windows.Win32.ITfTextEditSink",
            "Windows.Win32.ITfTextLayoutSink",
            "Windows.Win32.ITfKeyEventSink",
            "Windows.Win32.ITfKeystrokeMgr",
            "Windows.Win32.ITfThreadFocusSink",
            "Windows.Win32.ITfCompositionSink",
            "Windows.Win32.ITfActiveLanguageProfileNotifySink",
            "Windows.Win32.ITfDisplayAttributeProvider",
            "Windows.Win32.ITfCompartmentEventSink",
            "Windows.Win32.ITfEditSession",
            "Windows.Win32.IEnumTfDisplayAttributeInfo",
            "Windows.Win32.ITfDisplayAttributeInfo",
            "Windows.Win32.ITfCategoryMgr",
            "Windows.Win32.ITfInputProcessorProfileMgr",
            "Windows.Win32.UI.TextServices.CLSID_TF_CategoryMgr",
            "Windows.Win32.UI.TextServices.CLSID_TF_InputProcessorProfiles",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_CATEGORY_OF_TIP",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIP_KEYBOARD",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_SECUREMODE",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_UIELEMENTENABLED",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_COMLESS",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_WOW16",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_PROP_AUDIODATA",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_PROP_INKDATA",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_PROPSTYLE_CUSTOM",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_PROPSTYLE_STATIC",
            "Windows.Win32.UI.TextServices.GUID_TFCAT_PROPSTYLE_STATICCOMPACT",
            "Windows.Win32.UI.TextServices.TF_PROFILETYPE_INPUTPROCESSOR",
            "Windows.Win32.UI.TextServices.GUID_WEASEL_PROFILE",
            "Windows.Win32.UI.TextServices.GUID_PROP_ATTRIBUTE",
            "Windows.Win32.VARENUM",
            "Windows.Win32.UI.TextServices.TF_ANCHOR_START",
            "Windows.Win32.UI.TextServices.TF_ANCHOR_END",
            "Windows.Win32.UI.TextServices.TF_AE_NONE",
            "Windows.Win32.UI.TextServices.TF_AE_START",
            "Windows.Win32.UI.TextServices.TF_AE_END",
            "Windows.Win32.UI.TextServices.TF_LC_CREATE",
            "Windows.Win32.UI.TextServices.TF_LC_CHANGE",
            "Windows.Win32.UI.TextServices.TF_LC_DESTROY",
            "Windows.Win32.UI.TextServices.TF_GRAVITY_BACKWARD",
            "Windows.Win32.UI.TextServices.TF_GRAVITY_FORWARD",
            "Windows.Win32.UI.TextServices.TF_SD_BACKWARD",
            "Windows.Win32.UI.TextServices.TF_SD_FORWARD",
            "Windows.Win32.UI.TextServices.TF_LS_NONE",
            "Windows.Win32.UI.TextServices.TF_LS_SOLID",
            "Windows.Win32.UI.TextServices.TF_LS_DOT",
            "Windows.Win32.UI.TextServices.TF_LS_DASH",
            "Windows.Win32.UI.TextServices.TF_LS_SQUIGGLE",
            "Windows.Win32.UI.TextServices.TF_CT_NONE",
            "Windows.Win32.UI.TextServices.TF_CT_SYSCOLOR",
            "Windows.Win32.UI.TextServices.TF_CT_COLORREF",
            "Windows.Win32.UI.TextServices.TF_ATTR_INPUT",
            "Windows.Win32.UI.TextServices.TF_ATTR_TARGET_CONVERTED",
            "Windows.Win32.UI.TextServices.TF_ATTR_CONVERTED",
            "Windows.Win32.UI.TextServices.TF_ATTR_TARGET_NOTCONVERTED",
            "Windows.Win32.UI.TextServices.TF_ATTR_INPUT_ERROR",
            "Windows.Win32.UI.TextServices.TF_ATTR_FIXEDCONVERTED",
            "Windows.Win32.UI.TextServices.TF_ATTR_OTHER",
            "Windows.Win32.UI.TextServices.TS_AS_TEXT_CHANGE",
            "Windows.Win32.UI.TextServices.TS_AS_SEL_CHANGE",
            "Windows.Win32.UI.TextServices.TS_AS_LAYOUT_CHANGE",
            "Windows.Win32.UI.TextServices.TS_AS_ATTR_CHANGE",
            "Windows.Win32.UI.TextServices.TS_AS_STATUS_CHANGE",
            "Windows.Win32.UI.TextServices.TS_AS_ALL_SINKS",
            "Windows.Win32.UI.TextServices.TS_LF_SYNC",
            "Windows.Win32.UI.TextServices.TS_LF_READ",
            "Windows.Win32.UI.TextServices.TS_LF_READWRITE",
            "Windows.Win32.UI.TextServices.TS_SD_READONLY",
            "Windows.Win32.UI.TextServices.TS_SD_LOADING",
            "Windows.Win32.UI.TextServices.TS_SD_RESERVED",
            "Windows.Win32.UI.TextServices.TS_SD_TKBAUTOCORRECTENABLE",
            "Windows.Win32.UI.TextServices.TS_SD_TKBPREDICTIONENABLE",
            "Windows.Win32.UI.TextServices.TS_SD_UIINTEGRATIONENABLE",
            "Windows.Win32.UI.TextServices.TS_SD_INPUTPANEMANUALDISPLAYENABLE",
            "Windows.Win32.UI.TextServices.TS_SD_EMBEDDEDHANDWRITINGVIEW_ENABLED",
            "Windows.Win32.UI.TextServices.TS_SD_EMBEDDEDHANDWRITINGVIEW_VISIBLE",
            "Windows.Win32.UI.TextServices.TS_SD_MASKALL",
            "Windows.Win32.UI.TextServices.TS_SS_DISJOINTSEL",
            "Windows.Win32.UI.TextServices.TS_SS_REGIONS",
            "Windows.Win32.UI.TextServices.TS_SS_TRANSITORY",
            "Windows.Win32.UI.TextServices.TS_SS_NOHIDDENTEXT",
            "Windows.Win32.UI.TextServices.TS_SS_TKBAUTOCORRECTENABLE",
            "Windows.Win32.UI.TextServices.TS_SS_TKBPREDICTIONENABLE",
            "Windows.Win32.UI.TextServices.TS_SS_UWPCONTROL",
            "Windows.Win32.UI.TextServices.TS_ST_CORRECTION",
            "Windows.Win32.UI.TextServices.TS_IE_CORRECTION",
            "Windows.Win32.UI.TextServices.TS_IE_COMPOSITION",
            "Windows.Win32.UI.TextServices.TS_TC_CORRECTION",
            "Windows.Win32.UI.TextServices.TS_IAS_NOQUERY",
            "Windows.Win32.UI.TextServices.TS_IAS_QUERYONLY",
            "Windows.Win32.UI.TextServices.GXFPF_ROUND_NEAREST",
            "Windows.Win32.UI.TextServices.GXFPF_NEAREST",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_BACKWARDS",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_WANT_OFFSET",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_UPDATESTART",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_WANT_VALUE",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_WANT_END",
            "Windows.Win32.UI.TextServices.TS_ATTR_FIND_HIDDEN",
            "Windows.Win32.UI.TextServices.TS_CH_PRECEDING_DEL",
            "Windows.Win32.UI.TextServices.TS_CH_FOLLOWING_DEL",
            "Windows.Win32.UI.TextServices.TS_SHIFT_COUNT_HIDDEN",
            "Windows.Win32.UI.TextServices.TS_SHIFT_HALT_HIDDEN",
            "Windows.Win32.UI.TextServices.TS_SHIFT_HALT_VISIBLE",
            "Windows.Win32.UI.TextServices.TS_SHIFT_COUNT_ONLY",
            "Windows.Win32.UI.TextServices.TS_GTA_HIDDEN",
            "Windows.Win32.UI.TextServices.TS_GEA_HIDDEN",
            "Windows.Win32.UI.TextServices.TF_POPF_ALL",
            "Windows.Win32.UI.TextServices.TF_ES_ASYNCDONTCARE",
            "Windows.Win32.UI.TextServices.TF_ES_SYNC",
            "Windows.Win32.UI.TextServices.TF_ES_READ",
            "Windows.Win32.UI.TextServices.TF_ES_READWRITE",
            "Windows.Win32.UI.TextServices.TF_ES_ASYNC",
            "Windows.Win32.UI.TextServices.TF_SD_READONLY",
            "Windows.Win32.UI.TextServices.TF_SD_LOADING",
            "Windows.Win32.UI.TextServices.TF_SD_RESERVED",
            "Windows.Win32.UI.TextServices.TF_SD_TKBAUTOCORRECTENABLE",
            "Windows.Win32.UI.TextServices.TF_SD_TKBPREDICTIONENABLE",
            "Windows.Win32.UI.TextServices.TF_SD_UIINTEGRATIONENABLE",
            "Windows.Win32.UI.TextServices.TF_SD_INPUTPANEMANUALDISPLAYENABLE",
            "Windows.Win32.UI.TextServices.TF_SD_EMBEDDEDHANDWRITINGVIEW_ENABLED",
            "Windows.Win32.UI.TextServices.TF_SD_EMBEDDEDHANDWRITINGVIEW_VISIBLE",
            "Windows.Win32.UI.TextServices.TF_SD_MASKALL",
            "Windows.Win32.UI.TextServices.TF_SS_DISJOINTSEL",
            "Windows.Win32.UI.TextServices.TF_SS_REGIONS",
            "Windows.Win32.UI.TextServices.TF_SS_TRANSITORY",
            "Windows.Win32.UI.TextServices.TF_SS_TKBAUTOCORRECTENABLE",
            "Windows.Win32.UI.TextServices.TF_SS_TKBPREDICTIONENABLE",
            "Windows.Win32.UI.TextServices.TF_SS_UWPCONTROL",
            "Windows.Win32.UI.TextServices.TF_IAS_NOQUERY",
            "Windows.Win32.UI.TextServices.TF_IAS_QUERYONLY",
            "Windows.Win32.UI.TextServices.TF_IAS_NO_DEFAULT_COMPOSITION",
            "Windows.Win32.UI.TextServices.TF_GTP_INCL_TEXT",
            "Windows.Win32.UI.TextServices.TF_HF_OBJECT",
            "Windows.Win32.UI.TextServices.TF_TF_MOVESTART",
            "Windows.Win32.UI.TextServices.TF_TF_IGNOREEND",
            "Windows.Win32.UI.TextServices.TF_ST_CORRECTION",
            "Windows.Win32.UI.TextServices.TF_IE_CORRECTION",
            "Windows.Win32.UI.TextServices.TF_TU_CORRECTION",
            "Windows.Win32.UI.TextServices.TF_INVALID_COOKIE",
            "Weasel.GUID_WEASEL_DISPLAY_ATTRIBUTE",
            "Windows.Win32.CoCreateInstance",
            "Windows.Win32.CLSCTX",
            "Windows.Win32.GetModuleFileNameW",
            "Windows.Win32.GetModuleHandleExW",
            "Windows.Win32.GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS",
            "Windows.Win32.CLASS_E_CLASSNOTAVAILABLE",
            "Windows.Win32.CLASS_E_NOAGGREGATION",
            "Windows.Win32.E_NOTIMPL",
            "Windows.Win32.E_NOINTERFACE",
            "Windows.Win32.E_POINTER",
            "Windows.Win32.E_FAIL",
            "Windows.Win32.S_OK",
            "Windows.Win32.S_FALSE",
        ])
        .implements([
            "Windows.Win32.ITfLangBarItem",
            "Windows.Win32.ITfLangBarItemButton",
            "Windows.Win32.ITfSource",
            "Windows.Win32.IClassFactory",
            "Windows.Win32.ITfTextInputProcessor",
            "Windows.Win32.ITfTextInputProcessorEx",
            "Windows.Win32.ITfThreadMgrEventSink",
            "Windows.Win32.ITfTextEditSink",
            "Windows.Win32.ITfTextLayoutSink",
            "Windows.Win32.ITfKeyEventSink",
            "Windows.Win32.ITfKeystrokeMgr",
            "Windows.Win32.ITfThreadFocusSink",
            "Windows.Win32.ITfCompositionSink",
            "Windows.Win32.ITfActiveLanguageProfileNotifySink",
            "Windows.Win32.ITfDisplayAttributeProvider",
            "Windows.Win32.ITfCompartmentEventSink",
            "Windows.Win32.ITfEditSession",
            "Windows.Win32.IEnumTfDisplayAttributeInfo",
            "Windows.Win32.ITfDisplayAttributeInfo",
        ])
        .flat()
        .write();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manual_rdl.display());
    println!("cargo:rerun-if-changed={}", weasel_rdl.display());
    println!("cargo:rerun-if-changed={}", module_definition.display());
    println!(
        "cargo:rustc-cdylib-link-arg=/DEF:{}",
        module_definition.display()
    );
}

fn embed_icons() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let resource = root.join("icons.rc");
    println!("cargo:rerun-if-changed={}", resource.display());
    let definitions: Vec<String> = [
        ("WEASEL_ICON_PATH", "weasel.ico"),
        ("EN_DARK_PATH", "en-dark.ico"),
        ("EN_LIGHT_PATH", "en-light.ico"),
        ("ERROR_DARK_PATH", "error-dark.ico"),
        ("ERROR_LIGHT_PATH", "error-light.ico"),
        ("ZH_DARK_PATH", "zh-dark.ico"),
        ("ZH_LIGHT_PATH", "zh-light.ico"),
    ]
    .into_iter()
    .map(|(name, file)| {
        let path = root.join("../assets").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let path = path
            .to_str()
            .expect("icon path must be UTF-8")
            .replace('\\', "/");
        format!("{name}=\"{path}\"")
    })
    .collect();
    embed_resource::compile_for_cdylib(resource, &definitions)
        .manifest_required()
        .expect("failed to embed TIP language bar icons");
}
