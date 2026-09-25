#![allow(async_fn_in_trait)]
#![allow(dead_code)]

use tokio::sync::broadcast;
use weasel_common::{
    message::{
        ContextCommand, Envelope, InputKey, KeyEventResponse, LayoutUpdate, LogEvent, Ping, Pong,
        Shutdown, ShutdownResponse, envelope::Payload,
    },
    rpc::{RpcClient, RpcConnection, RpcError},
};

pub trait ClientProtocol {
    async fn ping(&self, text: &str) -> Result<Pong, RpcError>;
    async fn shutdown(&self, reason: &str) -> Result<ShutdownResponse, RpcError>;
    async fn process_translated_key(&self, event: InputKey) -> Result<KeyEventResponse, RpcError>;
    async fn context_command(&self, command: ContextCommand) -> Result<KeyEventResponse, RpcError>;
    async fn send_layout_update(&self, update: LayoutUpdate) -> Result<(), RpcError>;
}

impl ClientProtocol for RpcClient {
    async fn ping(&self, text: &str) -> Result<Pong, RpcError> {
        match self
            .request(Payload::Ping(Ping { text: text.into() }))
            .await?
            .payload
        {
            Some(Payload::Pong(value)) => Ok(value),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn shutdown(&self, reason: &str) -> Result<ShutdownResponse, RpcError> {
        match self
            .request(Payload::Shutdown(Shutdown {
                reason: reason.into(),
            }))
            .await?
            .payload
        {
            Some(Payload::ShutdownResponse(value)) => Ok(value),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn process_translated_key(&self, event: InputKey) -> Result<KeyEventResponse, RpcError> {
        match self.request(Payload::KeyEvent(event)).await?.payload {
            Some(Payload::KeyEventResponse(value)) => Ok(value),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn context_command(&self, command: ContextCommand) -> Result<KeyEventResponse, RpcError> {
        match self
            .request(Payload::ContextCommand(command))
            .await?
            .payload
        {
            Some(Payload::KeyEventResponse(value)) => Ok(value),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn send_layout_update(&self, update: LayoutUpdate) -> Result<(), RpcError> {
        self.publish(Payload::LayoutUpdate(update))
    }
}

pub trait ConnectionProtocol {
    async fn send_pong(&self, request_id: u64, text: &str) -> Result<(), RpcError>;
    async fn send_key_event_response(
        &self,
        request_id: u64,
        response: KeyEventResponse,
    ) -> Result<(), RpcError>;
    async fn send_shutdown_response(
        &self,
        request_id: u64,
        response: ShutdownResponse,
    ) -> Result<(), RpcError>;
    async fn send_log_event(&self, level: &str, text: &str) -> Result<(), RpcError>;
}

impl ConnectionProtocol for RpcConnection {
    async fn send_pong(&self, request_id: u64, text: &str) -> Result<(), RpcError> {
        send(self, request_id, Payload::Pong(Pong { text: text.into() })).await
    }

    async fn send_key_event_response(
        &self,
        request_id: u64,
        response: KeyEventResponse,
    ) -> Result<(), RpcError> {
        send(self, request_id, Payload::KeyEventResponse(response)).await
    }

    async fn send_shutdown_response(
        &self,
        request_id: u64,
        response: ShutdownResponse,
    ) -> Result<(), RpcError> {
        send(self, request_id, Payload::ShutdownResponse(response)).await
    }

    async fn send_log_event(&self, level: &str, text: &str) -> Result<(), RpcError> {
        send(
            self,
            0,
            Payload::LogEvent(LogEvent {
                level: level.into(),
                text: text.into(),
            }),
        )
        .await
    }
}

async fn send(
    connection: &RpcConnection,
    request_id: u64,
    payload: Payload,
) -> Result<(), RpcError> {
    connection
        .send(&Envelope {
            request_id,
            payload: Some(payload),
        })
        .await
}

pub async fn recv_key(
    receiver: &mut broadcast::Receiver<Envelope>,
    event_only: bool,
) -> KeyEventResponse {
    loop {
        let envelope = receiver.recv().await.unwrap();
        if event_only && envelope.request_id != 0 {
            continue;
        }
        if let Some(Payload::KeyEventResponse(value)) = envelope.payload {
            return value;
        }
    }
}

pub async fn recv_log(receiver: &mut broadcast::Receiver<Envelope>) -> LogEvent {
    loop {
        if let Some(Payload::LogEvent(value)) = receiver.recv().await.unwrap().payload {
            return value;
        }
    }
}
