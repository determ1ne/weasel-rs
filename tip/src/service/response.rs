//! 在分配文本存储缓冲区或请求 TSF 编辑会话前校验进程外响应。
use super::*;

/// 单个响应文本字段允许的最大 UTF-8 字节数。
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// 判断响应是否包含需要触发宿主编辑的状态或一次性效果。
///
/// 单纯确认且没有状态不代表取消；提交文本和打开表情面板则无需冗余状态快照。
pub(super) fn has_edit_payload(response: &KeyEventResponse) -> bool {
    response.state_updated || !response.commit_text.is_empty() || response.open_emoji_panel
}

/// 验证文本大小及组合光标是否落在 UTF-16 字符边界。
///
/// 引擎数据跨越进程边界，非法长度或落在代理项中间的光标均以 `E_FAIL` 拒绝。
pub(super) fn validate(response: &KeyEventResponse) -> Result<()> {
    if response.composition.len() > MAX_TEXT_BYTES
        || response.commit_text.len() > MAX_TEXT_BYTES
        || response
            .raw_input
            .as_ref()
            .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
    {
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
