//! 跨进程消息的字段级合法性检查。
//!
//! 本模块只验证单个 protobuf 值的结构和边界，不决定它属于 request、response 还是 event；
//! 消息方向仍由父级映射层负责。

use crate::message as m;

use super::super::{RpcError, limits::protocol};

/// 校验用户通知的必填字段、UTF-8 字节长度、严重级别和 NUL 字符。
pub(super) fn notification(value: &m::UserNotification) -> Result<(), RpcError> {
    if value.source.is_empty()
        || value.code.is_empty()
        || value.title.is_empty()
        || value.source.len() > protocol::MAX_NOTIFICATION_SOURCE_BYTES
        || value.code.len() > protocol::MAX_NOTIFICATION_CODE_BYTES
        || value.title.len() > protocol::MAX_NOTIFICATION_TITLE_BYTES
        || value.message.len() > protocol::MAX_NOTIFICATION_MESSAGE_BYTES
        || value.details.len() > protocol::MAX_NOTIFICATION_DETAILS_BYTES
        || !matches!(
            m::UserNotificationSeverity::try_from(value.severity),
            Ok(m::UserNotificationSeverity::Info
                | m::UserNotificationSeverity::Warning
                | m::UserNotificationSeverity::Error)
        )
        || [
            &value.source,
            &value.code,
            &value.title,
            &value.message,
            &value.details,
        ]
        .iter()
        .any(|text| text.contains('\0'))
    {
        return Err(invalid("invalid user notification"));
    }
    Ok(())
}

/// 要求输入上下文令牌存在，且上下文、连接世代和 composition 世代均非零。
pub(super) fn token(value: &Option<m::ContextToken>) -> Result<(), RpcError> {
    if !value.as_ref().is_some_and(|token| {
        token.context_id != 0 && token.connection_epoch != 0 && token.generation != 0
    }) {
        return Err(invalid("nonzero input context token required"));
    }
    Ok(())
}

/// 校验上下文命令的令牌、动作枚举及动作所需参数。
pub(super) fn context(value: &m::ContextCommand) -> Result<(), RpcError> {
    token(&value.token)?;
    if matches!(
        m::ContextAction::try_from(value.action),
        Err(_) | Ok(m::ContextAction::Unspecified)
    ) {
        return Err(invalid("unknown context action"));
    }
    if value.action == m::ContextAction::SetAscii as i32 && value.ascii_mode.is_none() {
        return Err(invalid("SetAscii requires ascii_mode"));
    }
    Ok(())
}

/// 校验渲染交互动作是否为已定义且非未指定值。
pub(super) fn interaction(value: &m::RendererEvent) -> Result<(), RpcError> {
    if matches!(
        m::RendererEventAction::try_from(value.action),
        Err(_) | Ok(m::RendererEventAction::Unspecified)
    ) {
        return Err(invalid("unknown renderer action"));
    }
    Ok(())
}

/// 校验输入状态的长度、候选选择和 UTF-16 光标边界。
pub(super) fn input_state(value: &m::InputState) -> Result<(), RpcError> {
    if value
        .raw_input
        .as_ref()
        .is_some_and(|text| text.len() > protocol::MAX_RAW_INPUT_BYTES)
    {
        return Err(invalid("raw input too long"));
    }
    if value.cursor_utf16 as usize > value.preedit.encode_utf16().count()
        || value.candidates.len() > protocol::MAX_CANDIDATES_PER_PAGE
        || (value.candidates.is_empty() && value.selected != 0)
        || (!value.candidates.is_empty() && value.selected as usize >= value.candidates.len())
    {
        return Err(invalid("invalid composition cursor or candidate selection"));
    }

    let mut offset = 0;
    for character in value.preedit.chars() {
        if offset == value.cursor_utf16 as usize {
            return Ok(());
        }
        offset += character.len_utf16();
    }
    if offset != value.cursor_utf16 as usize {
        return Err(invalid("cursor splits UTF-16 surrogate pair"));
    }
    Ok(())
}

fn invalid(message: &str) -> RpcError {
    RpcError::Protocol(message.into())
}
