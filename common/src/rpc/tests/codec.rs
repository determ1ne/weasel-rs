use super::*;

/// VERSION=1、空请求体的旧 RpcFrame fixture；验证旧 wire 格式不会被当前协议误收。
const LEGACY_EMPTY_REQUEST: &[u8] = &[8, 1, 18, 0];

#[test]
fn rejects_legacy_and_wrong_version() {
    assert!(decode(LEGACY_EMPTY_REQUEST).is_err());
    let mut frame = handshake::hello(m::PeerRole::Tip, 1);
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
        raw_input: Some("a😀".into()),
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
    for id in [super::super::limits::protocol::EVENT_REQUEST_ID, 42] {
        let original = m::Envelope {
            request_id: id,
            payload: Some(P::KeyEventResponse(update.clone())),
        };
        let bytes = pack(&original).unwrap().encode_to_vec();
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
            candidates: vec![
                m::Candidate::default();
                super::super::limits::protocol::MAX_CANDIDATES_PER_PAGE + 1
            ],
            ..Default::default()
        },
    ] {
        assert!(input_state(&state).is_err());
    }
}

#[test]
fn validates_utf16_boundaries() {
    let mut s = m::InputState {
        preedit: "😀".into(),
        cursor_utf16: 1,
        ..Default::default()
    };
    assert!(input_state(&s).is_err());
    s.cursor_utf16 = 2;
    assert!(input_state(&s).is_ok());
}

#[test]
fn rejects_request_event_confusion_and_missing_key_token() {
    assert!(
        pack(&m::Envelope {
            request_id: super::super::limits::protocol::EVENT_REQUEST_ID,
            payload: Some(P::Ping(m::Ping::default()))
        })
        .is_err()
    );
    assert!(
        pack(&m::Envelope {
            request_id: super::super::limits::protocol::FIRST_REQUEST_ID,
            payload: Some(P::KeyEvent(m::InputKey::default()))
        })
        .is_err()
    );
}
