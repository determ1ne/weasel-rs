//! Protobuf messages shared by the tip and out-of-process components.

include!(concat!(env!("OUT_DIR"), "/weasel.message.rs"));

// In-process dispatch model, deliberately NOT a prost Message. Only RpcFrame
// is serializable over a pipe; these adapters are not an old-wire decoder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Envelope {
    pub request_id: u64,
    pub payload: Option<envelope::Payload>,
}

/// TSF-to-engine domain request. Native fields are metadata, never translated
/// on the engine thread. A missing keycode/test request is rejected at the wire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyEvent {
    pub virtual_key: u32,
    pub lparam: i64,
    pub key_up: bool,
    pub test: bool,
    pub keycode: Option<i32>,
    pub modifiers: i32,
    pub token: Option<ContextToken>,
}

/// Local editing adapter. On the wire, InputResult separates optional state
/// from one-shot effects; no legacy flat-response decoder exists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyEventResponse {
    pub eaten: bool,
    pub commit_text: String,
    pub composition: String,
    pub candidates: Vec<Candidate>,
    pub selected_candidate: u32,
    pub composition_cursor: u32,
    pub state_updated: bool,
    pub composing: bool,
    pub page_start: u32,
    pub can_page_previous: bool,
    pub can_page_next: bool,
    pub open_emoji_panel: bool,
    pub token: Option<ContextToken>,
    pub revision: u64,
    pub ascii_mode: Option<bool>,
}
pub mod envelope {
    use super::*;
    #[derive(Clone, Debug, PartialEq)]
    pub enum Payload {
        QueryConfig(QueryConfig),
        ConfigValue(ConfigValue),
        Ping(Ping),
        Pong(Pong),
        LogEvent(LogEvent),
        KeyEvent(KeyEvent),
        KeyEventResponse(KeyEventResponse),
        Shutdown(Shutdown),
        ShutdownResponse(ShutdownResponse),
        RenderSnapshot(RenderSnapshot),
        RendererEvent(RendererEvent),
        LayoutUpdate(LayoutUpdate),
        ContextCommand(ContextCommand),
        Failure(Failure),
        OpenInput(OpenInput),
        InputOpened(InputOpened),
    }
}
