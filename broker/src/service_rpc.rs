//! Broker 对受管子进程控制 RPC 的类型化封装。
//!
//! 此模块将协议负载与预期响应配对；服务端返回其他类型的负载时统一报告意外响应。

use weasel_common::{
    message::{IdentifyService, ServiceIdentity, Shutdown, ShutdownResponse, envelope::Payload},
    rpc::{RpcClient, RpcError},
};

/// 请求对端标识自身，并提取其服务身份。
///
/// 传输错误原样传播；若响应负载不是服务身份，则返回 [`RpcError::UnexpectedResponse`]。
pub async fn identify(client: &RpcClient) -> Result<ServiceIdentity, RpcError> {
    match client
        .request(Payload::IdentifyService(IdentifyService {}))
        .await?
        .payload
    {
        Some(Payload::ServiceIdentity(identity)) => Ok(identity),
        _ => Err(RpcError::UnexpectedResponse),
    }
}

/// 请求关闭受管服务，并返回对端的关闭结果。
///
/// `reason` 会转换为协议中的关闭原因。传输错误原样传播；响应负载类型不符时返回
/// [`RpcError::UnexpectedResponse`]。
pub async fn shutdown(
    client: &RpcClient,
    reason: impl Into<String>,
) -> Result<ShutdownResponse, RpcError> {
    match client
        .request(Payload::Shutdown(Shutdown {
            reason: reason.into(),
        }))
        .await?
        .payload
    {
        Some(Payload::ShutdownResponse(response)) => Ok(response),
        _ => Err(RpcError::UnexpectedResponse),
    }
}
