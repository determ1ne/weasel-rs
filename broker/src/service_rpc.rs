//! Broker 对受管子进程控制协议的类型化封装。

use weasel_common::{
    message::{IdentifyService, ServiceIdentity, Shutdown, ShutdownResponse, envelope::Payload},
    rpc::{RpcClient, RpcError},
};

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
