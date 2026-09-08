//! The strict wire boundary. Domain dispatch never decodes the legacy envelope.
use super::RpcError;
use crate::message::{self as m, envelope::Payload as P};
use prost::Message;

// Bump only when the shared transport/TIP contract becomes incompatible.
// Out-of-process components are updated together.
pub const VERSION: u32 = 3;
fn invalid(text: &str) -> RpcError {
    RpcError::Protocol(text.into())
}

pub fn hello(role: m::PeerRole, instance_id: u64) -> m::RpcFrame {
    m::RpcFrame {
        protocol_version: VERSION,
        body: Some(m::rpc_frame::Body::Hello(m::Hello {
            role: role as i32,
            instance_id,
        })),
    }
}

pub fn decode(bytes: &[u8]) -> Result<m::RpcFrame, RpcError> {
    let frame = m::RpcFrame::decode(bytes)?;
    if frame.protocol_version != VERSION {
        return Err(invalid("incompatible protocol version"));
    }
    if frame.body.is_none() {
        return Err(invalid("missing frame body"));
    }
    Ok(frame)
}

pub fn encode(envelope: &m::Envelope) -> Result<m::RpcFrame, RpcError> {
    use m::{
        event::Notification as E, request::Operation as Q, response::Result as R,
        rpc_frame::Body as B,
    };
    let id = envelope.request_id;
    let payload = envelope
        .payload
        .clone()
        .ok_or_else(|| invalid("missing payload"))?;
    let body = match payload {
        P::IdentifyService(v) if id != 0 => B::Request(m::Request {
            id,
            operation: Some(Q::IdentifyService(v)),
        }),
        P::ServiceIdentity(v) if id != 0 => B::Response(m::Response {
            id,
            result: Some(R::ServiceIdentity(v)),
        }),
        P::UserNotification(v) if id != 0 => {
            validate_notification(&v)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::UserNotification(v)),
            })
        }
        P::QueryConfig(v) if id != 0 => B::Request(m::Request {
            id,
            operation: Some(Q::QueryConfig(v)),
        }),
        P::ConfigValue(v) if id != 0 => B::Response(m::Response {
            id,
            result: Some(R::ConfigValue(v)),
        }),
        P::Ping(v) if id != 0 => B::Request(m::Request {
            id,
            operation: Some(Q::Ping(v)),
        }),
        P::OpenInput(v) if id != 0 => {
            validate_token(&v.token)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::OpenInput(v)),
            })
        }
        P::InputOpened(v) if id != 0 => {
            validate_token(&v.token)?;
            B::Response(m::Response {
                id,
                result: Some(R::InputOpened(v)),
            })
        }
        P::KeyEvent(v) if id != 0 => {
            validate_token(&v.token)?;
            if v.test {
                return Err(invalid("test key events are local to TIP"));
            }
            let keycode = v
                .keycode
                .ok_or_else(|| invalid("keycode must be translated by TIP"))?;
            B::Request(m::Request {
                id,
                operation: Some(Q::Key(m::InputKey {
                    token: v.token,
                    keycode,
                    modifiers: v.modifiers,
                    released: v.key_up,
                    virtual_key: v.virtual_key,
                    native_lparam: v.lparam,
                })),
            })
        }
        P::ContextCommand(v) if id != 0 => {
            validate_context(&v)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::Context(v)),
            })
        }
        P::Shutdown(v) if id != 0 => B::Request(m::Request {
            id,
            operation: Some(Q::Shutdown(v)),
        }),
        P::Pong(v) if id != 0 => B::Response(m::Response {
            id,
            result: Some(R::Pong(v)),
        }),
        P::ShutdownResponse(v) if id != 0 => B::Response(m::Response {
            id,
            result: Some(R::Shutdown(v)),
        }),
        P::Failure(v) if id != 0 => B::Response(m::Response {
            id,
            result: Some(R::Failure(v)),
        }),
        P::KeyEventResponse(v) => {
            let input = pack_input(v)?;
            if id == 0 {
                B::Event(m::Event {
                    notification: Some(E::Input(input)),
                })
            } else {
                B::Response(m::Response {
                    id,
                    result: Some(R::Input(input)),
                })
            }
        }
        P::LogEvent(v) if id == 0 => B::Event(m::Event {
            notification: Some(E::Diagnostic(v)),
        }),
        P::RenderSnapshot(v) if id == 0 => B::Event(m::Event {
            notification: Some(E::Render(v)),
        }),
        P::RendererEvent(v) if id == 0 => {
            validate_interaction(&v)?;
            B::Event(m::Event {
                notification: Some(E::Interaction(v)),
            })
        }
        P::LayoutUpdate(v) if id == 0 => B::Event(m::Event {
            notification: Some(E::Layout(v)),
        }),
        _ => return Err(invalid("payload is not valid for request/event ID")),
    };
    Ok(m::RpcFrame {
        protocol_version: VERSION,
        body: Some(body),
    })
}

pub fn unpack(frame: m::RpcFrame) -> Result<m::Envelope, RpcError> {
    use m::{
        event::Notification as E, request::Operation as Q, response::Result as R,
        rpc_frame::Body as B,
    };
    let (id, payload) = match frame.body.ok_or_else(|| invalid("missing body"))? {
        B::Hello(_) => return Err(invalid("duplicate hello")),
        B::Request(v) => {
            if v.id == 0 {
                return Err(invalid("zero request ID"));
            }
            (
                v.id,
                match v.operation.ok_or_else(|| invalid("missing operation"))? {
                    Q::Ping(v) => P::Ping(v),
                    Q::IdentifyService(v) => P::IdentifyService(v),
                    Q::UserNotification(v) => {
                        validate_notification(&v)?;
                        P::UserNotification(v)
                    }
                    Q::QueryConfig(v) => P::QueryConfig(v),
                    Q::Shutdown(v) => P::Shutdown(v),
                    Q::OpenInput(v) => {
                        validate_token(&v.token)?;
                        P::OpenInput(v)
                    }
                    Q::Context(v) => {
                        validate_context(&v)?;
                        P::ContextCommand(v)
                    }
                    Q::Key(v) => {
                        validate_token(&v.token)?;
                        P::KeyEvent(m::KeyEvent {
                            token: v.token,
                            keycode: Some(v.keycode),
                            modifiers: v.modifiers,
                            key_up: v.released,
                            virtual_key: v.virtual_key,
                            lparam: v.native_lparam,
                            test: false,
                        })
                    }
                },
            )
        }
        B::Response(v) => {
            if v.id == 0 {
                return Err(invalid("zero response ID"));
            }
            (
                v.id,
                match v.result.ok_or_else(|| invalid("missing result"))? {
                    R::Pong(v) => P::Pong(v),
                    R::ServiceIdentity(v) => P::ServiceIdentity(v),
                    R::ConfigValue(v) => P::ConfigValue(v),
                    R::Shutdown(v) => P::ShutdownResponse(v),
                    R::InputOpened(v) => {
                        validate_token(&v.token)?;
                        P::InputOpened(v)
                    }
                    R::Input(v) => P::KeyEventResponse(unpack_input(v)?),
                    R::Failure(v) => P::Failure(v),
                },
            )
        }
        B::Event(v) => (
            0,
            match v
                .notification
                .ok_or_else(|| invalid("missing notification"))?
            {
                E::Diagnostic(v) => P::LogEvent(v),
                E::Input(v) => P::KeyEventResponse(unpack_input(v)?),
                E::Render(v) => P::RenderSnapshot(v),
                E::Layout(v) => P::LayoutUpdate(v),
                E::Interaction(v) => {
                    validate_interaction(&v)?;
                    P::RendererEvent(v)
                }
            },
        ),
    };
    Ok(m::Envelope {
        request_id: id,
        payload: Some(payload),
    })
}

fn validate_notification(v: &m::UserNotification) -> Result<(), RpcError> {
    if v.source.is_empty()
        || v.code.is_empty()
        || v.title.is_empty()
        || v.source.len() > 64
        || v.code.len() > 128
        || v.title.len() > 512
        || v.message.len() > 2048
        || v.details.len() > 16384
        || !matches!(
            m::UserNotificationSeverity::try_from(v.severity),
            Ok(m::UserNotificationSeverity::Info
                | m::UserNotificationSeverity::Warning
                | m::UserNotificationSeverity::Error)
        )
        || [&v.source, &v.code, &v.title, &v.message, &v.details]
            .iter()
            .any(|s| s.contains('\0'))
    {
        return Err(invalid("invalid user notification"));
    }
    Ok(())
}

fn validate_token(v: &Option<m::ContextToken>) -> Result<(), RpcError> {
    if !v
        .as_ref()
        .is_some_and(|t| t.context_id != 0 && t.connection_epoch != 0 && t.generation != 0)
    {
        return Err(invalid("nonzero input context token required"));
    }
    Ok(())
}
fn validate_context(v: &m::ContextCommand) -> Result<(), RpcError> {
    validate_token(&v.token)?;
    if matches!(
        m::ContextAction::try_from(v.action),
        Err(_) | Ok(m::ContextAction::Unspecified)
    ) {
        return Err(invalid("unknown context action"));
    }
    if v.action == m::ContextAction::SetAscii as i32 && v.ascii_mode.is_none() {
        return Err(invalid("SetAscii requires ascii_mode"));
    }
    Ok(())
}
fn validate_interaction(v: &m::RendererEvent) -> Result<(), RpcError> {
    if matches!(
        m::RendererEventAction::try_from(v.action),
        Err(_) | Ok(m::RendererEventAction::Unspecified)
    ) {
        return Err(invalid("unknown renderer action"));
    }
    Ok(())
}
fn validate_state(v: &m::InputState) -> Result<(), RpcError> {
    if v.cursor_utf16 as usize > v.preedit.encode_utf16().count()
        || v.candidates.len() > 256
        || (v.candidates.is_empty() && v.selected != 0)
        || (!v.candidates.is_empty() && v.selected as usize >= v.candidates.len())
    {
        return Err(invalid("invalid composition cursor or candidate selection"));
    }
    let mut offset = 0;
    for ch in v.preedit.chars() {
        if offset == v.cursor_utf16 as usize {
            return Ok(());
        }
        offset += ch.len_utf16();
    }
    if offset != v.cursor_utf16 as usize {
        return Err(invalid("cursor splits UTF-16 surrogate pair"));
    }
    Ok(())
}
fn pack_input(v: m::KeyEventResponse) -> Result<m::InputResult, RpcError> {
    let state = v.state_updated.then_some(m::InputState {
        preedit: v.composition,
        cursor_utf16: v.composition_cursor,
        composing: v.composing,
        candidates: v.candidates,
        selected: v.selected_candidate,
        page_start: v.page_start,
        has_previous: v.can_page_previous,
        has_next: v.can_page_next,
    });
    if let Some(state) = &state {
        validate_state(state)?;
    }
    Ok(m::InputResult {
        token: v.token,
        revision: v.revision,
        handled: v.eaten,
        state,
        effects: (!v.commit_text.is_empty() || v.open_emoji_panel).then_some(m::EditEffects {
            commit: v.commit_text,
            open_emoji_panel: v.open_emoji_panel,
        }),
        ascii_mode: v.ascii_mode,
    })
}
fn unpack_input(v: m::InputResult) -> Result<m::KeyEventResponse, RpcError> {
    let mut result = m::KeyEventResponse {
        token: v.token,
        revision: v.revision,
        eaten: v.handled,
        ascii_mode: v.ascii_mode,
        ..Default::default()
    };
    if let Some(s) = v.state {
        validate_state(&s)?;
        result.state_updated = true;
        result.composition = s.preedit;
        result.composition_cursor = s.cursor_utf16;
        result.composing = s.composing;
        result.candidates = s.candidates;
        result.selected_candidate = s.selected;
        result.page_start = s.page_start;
        result.can_page_previous = s.has_previous;
        result.can_page_next = s.has_next;
    }
    if let Some(e) = v.effects {
        result.commit_text = e.commit;
        result.open_emoji_panel = e.open_emoji_panel;
    }
    Ok(result)
}

pub async fn write_hello<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    role: m::PeerRole,
    instance: u64,
) -> Result<(), RpcError> {
    Ok(crate::framing::write(writer, &hello(role, instance)).await?)
}
pub async fn read_hello<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<m::Hello, RpcError> {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::framing::read(reader),
    )
    .await
    .map_err(|_| RpcError::Timeout)??
    .ok_or(RpcError::Disconnected)?;
    match decode(&bytes)?.body {
        Some(m::rpc_frame::Body::Hello(v))
            if m::PeerRole::try_from(v.role).is_ok() && v.instance_id != 0 =>
        {
            Ok(v)
        }
        _ => Err(invalid("hello required before messages")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_legacy_and_wrong_version() {
        assert!(decode(&[8, 1, 18, 0]).is_err());
        let mut frame = hello(m::PeerRole::Tip, 1);
        frame.protocol_version = 999;
        assert!(decode(&frame.encode_to_vec()).is_err());
    }
    #[test]
    fn effects_are_distinct_from_state() {
        let v = m::KeyEventResponse {
            commit_text: "字".into(),
            ..Default::default()
        };
        let packed = pack_input(v.clone()).unwrap();
        assert!(packed.state.is_none());
        assert_eq!(unpack_input(packed).unwrap(), v);
        let empty = m::KeyEventResponse {
            state_updated: true,
            ..Default::default()
        };
        assert!(pack_input(empty).unwrap().state.is_some());
    }

    #[test]
    fn input_transaction_round_trip_preserves_context_paging_and_effects() {
        let update = m::KeyEventResponse {
            token: Some(m::ContextToken {
                context_id: 7,
                connection_epoch: 8,
                generation: 9,
            }),
            revision: 10,
            eaten: true,
            state_updated: true,
            composing: true,
            composition: "a😀".into(),
            composition_cursor: 3,
            candidates: vec![m::Candidate {
                text: "字".into(),
                comment: "注".into(),
            }],
            page_start: 12,
            can_page_previous: true,
            can_page_next: true,
            commit_text: "前".into(),
            open_emoji_panel: true,
            ascii_mode: Some(false),
            ..Default::default()
        };
        for id in [0, 42] {
            let original = m::Envelope {
                request_id: id,
                payload: Some(P::KeyEventResponse(update.clone())),
            };
            let bytes = encode(&original).unwrap().encode_to_vec();
            assert_eq!(unpack(decode(&bytes).unwrap()).unwrap(), original);
        }
    }

    #[test]
    fn rejects_missing_operations_and_invalid_input_state() {
        assert!(
            unpack(m::RpcFrame {
                protocol_version: VERSION,
                body: Some(m::rpc_frame::Body::Request(m::Request {
                    id: 1,
                    operation: None,
                }))
            })
            .is_err()
        );
        for state in [
            m::InputState {
                selected: 1,
                ..Default::default()
            },
            m::InputState {
                cursor_utf16: 1,
                ..Default::default()
            },
            m::InputState {
                candidates: vec![m::Candidate::default(); 257],
                ..Default::default()
            },
        ] {
            assert!(validate_state(&state).is_err());
        }
    }
    #[test]
    fn validates_utf16_boundaries() {
        let mut s = m::InputState {
            preedit: "😀".into(),
            cursor_utf16: 1,
            ..Default::default()
        };
        assert!(validate_state(&s).is_err());
        s.cursor_utf16 = 2;
        assert!(validate_state(&s).is_ok());
    }
    #[test]
    fn rejects_request_event_confusion_and_legacy_key() {
        assert!(
            encode(&m::Envelope {
                request_id: 0,
                payload: Some(P::Ping(m::Ping::default()))
            })
            .is_err()
        );
        assert!(
            encode(&m::Envelope {
                request_id: 1,
                payload: Some(P::KeyEvent(m::KeyEvent::default()))
            })
            .is_err()
        );
    }
}
