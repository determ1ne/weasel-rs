//! 服务端握手角色和业务消息方向的协议策略。
//!
//! 角色由对端在 hello 中自报，只用于约束端点上的合法消息集合，不能替代 Named Pipe ACL
//! 或进程身份验证。

use crate::message::{Envelope, PeerRole, envelope::Payload};

use super::super::RpcError;

/// 校验本端是否允许声明为 `peer` 的对端接入。
pub(super) fn validate_peer_role(
    local: PeerRole,
    peer: i32,
    allow_unspecified: bool,
) -> Result<PeerRole, RpcError> {
    let peer =
        PeerRole::try_from(peer).map_err(|_| RpcError::Protocol("unknown peer role".into()))?;
    let allowed = match local {
        PeerRole::Server => {
            matches!(peer, PeerRole::Tip | PeerRole::Broker)
                || (allow_unspecified && peer == PeerRole::Unspecified)
        }
        PeerRole::Renderer => matches!(peer, PeerRole::Server | PeerRole::Broker),
        PeerRole::Broker => matches!(peer, PeerRole::Renderer | PeerRole::Server),
        _ => false,
    };
    if allowed {
        Ok(peer)
    } else {
        Err(RpcError::Protocol(
            "peer role is not allowed on this endpoint".into(),
        ))
    }
}

/// 校验已握手连接上的消息是否符合本端与对端角色组合。
pub(super) fn validate_business(
    local: PeerRole,
    peer: PeerRole,
    message: Envelope,
) -> Result<Envelope, RpcError> {
    let allowed = match (local, peer, message.payload.as_ref()) {
        (
            PeerRole::Broker,
            PeerRole::Renderer | PeerRole::Server,
            Some(Payload::UserNotification(_) | Payload::QueryConfig(_)),
        ) => true,
        (
            PeerRole::Server | PeerRole::Renderer,
            PeerRole::Broker,
            Some(Payload::Ping(_) | Payload::Shutdown(_) | Payload::IdentifyService(_)),
        ) => true,
        (
            PeerRole::Server,
            PeerRole::Tip,
            Some(
                Payload::Ping(_)
                | Payload::OpenInput(_)
                | Payload::KeyEvent(_)
                | Payload::ContextCommand(_)
                | Payload::LayoutUpdate(_)
                | Payload::LogEvent(_),
            ),
        ) => true,
        // 旧测试角色获得 TIP 与 broker 请求的并集，但永远不能发送响应。
        (
            PeerRole::Server,
            PeerRole::Unspecified,
            Some(
                Payload::Ping(_)
                | Payload::Shutdown(_)
                | Payload::OpenInput(_)
                | Payload::KeyEvent(_)
                | Payload::ContextCommand(_)
                | Payload::LayoutUpdate(_)
                | Payload::LogEvent(_),
            ),
        ) => true,
        (
            PeerRole::Renderer,
            PeerRole::Server,
            Some(Payload::Ping(_) | Payload::RenderSnapshot(_) | Payload::QueryConfig(_)),
        ) => true,
        _ => false,
    };
    if allowed {
        Ok(message)
    } else {
        Err(RpcError::Protocol(
            "message is not allowed for peer role".into(),
        ))
    }
}
