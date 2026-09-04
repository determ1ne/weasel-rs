//! Validate out-of-process data before allocating text-store buffers or editing.
use super::*;

const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Acknowledgements without state are not cancellation. Conversely, one-shot
/// effects do not require a redundant state snapshot to be applied.
pub(super) fn has_edit_payload(response: &KeyEventResponse) -> bool {
    response.state_updated || !response.commit_text.is_empty() || response.open_emoji_panel
}

pub(super) fn validate(response: &KeyEventResponse) -> Result<()> {
    if response.composition.len() > MAX_TEXT_BYTES || response.commit_text.len() > MAX_TEXT_BYTES {
        return Err(Error::from_hresult(boundary::E_FAIL));
    }
    let cursor = response.composition_cursor as usize;
    let mut offset = 0;
    for ch in response.composition.chars() {
        if offset == cursor {
            return Ok(());
        }
        offset += ch.len_utf16();
    }
    if offset == cursor {
        Ok(())
    } else {
        Err(Error::from_hresult(boundary::E_FAIL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_do_not_require_a_state_snapshot_and_ack_does_not_cancel() {
        assert!(!has_edit_payload(&KeyEventResponse::default()));
        for response in [
            KeyEventResponse {
                commit_text: "字".into(),
                ..Default::default()
            },
            KeyEventResponse {
                open_emoji_panel: true,
                ..Default::default()
            },
            KeyEventResponse {
                state_updated: true,
                ..Default::default()
            },
        ] {
            assert!(has_edit_payload(&response));
        }
    }

    #[test]
    fn cursor_must_be_a_utf16_character_boundary() {
        let mut response = KeyEventResponse {
            composition: "a😀b".into(),
            ..Default::default()
        };
        for cursor in [0, 1, 3, 4] {
            response.composition_cursor = cursor;
            assert!(validate(&response).is_ok());
        }
        for cursor in [2, 5, u32::MAX] {
            response.composition_cursor = cursor;
            assert!(validate(&response).is_err());
        }
    }

    #[test]
    fn oversized_commits_are_rejected() {
        let response = KeyEventResponse {
            commit_text: "x".repeat(MAX_TEXT_BYTES + 1),
            ..Default::default()
        };
        assert!(validate(&response).is_err());
    }
}
