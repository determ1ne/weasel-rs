//! 每条 RPC 连接开始时交换的 hello 帧。

use crate::message as m;

use super::{VERSION, decode};
use crate::rpc::{RpcError, limits::runtime};

/// 构造包含本端角色和进程生命周期实例标识的握手帧。
pub(super) fn hello(role: m::PeerRole, instance_id: u64) -> m::RpcFrame {
    m::RpcFrame {
        protocol_version: VERSION,
        body: Some(m::rpc_frame::Body::Hello(m::Hello {
            role: role as i32,
            instance_id,
        })),
    }
}

/// 在 RPC 管道上写入本端 hello。
pub(in crate::rpc) async fn write<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    role: m::PeerRole,
    instance_id: u64,
) -> Result<(), RpcError> {
    Ok(crate::data_frame::write(writer, &hello(role, instance_id)).await?)
}

/// 读取并验证对端 hello。
///
/// hello 必须是连接上的首个帧，并在统一握手期限内到达；角色枚举和实例标识也必须有效。
pub(in crate::rpc) async fn read<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<m::Hello, RpcError> {
    let bytes = tokio::time::timeout(runtime::HANDSHAKE_TIMEOUT, crate::data_frame::read(reader))
        .await
        .map_err(|_| RpcError::Timeout)??
        .ok_or(RpcError::Disconnected)?;
    match decode(&bytes)?.body {
        Some(m::rpc_frame::Body::Hello(value))
            if m::PeerRole::try_from(value.role).is_ok() && value.instance_id != 0 =>
        {
            Ok(value)
        }
        _ => Err(RpcError::Protocol("hello required before messages".into())),
    }
}
