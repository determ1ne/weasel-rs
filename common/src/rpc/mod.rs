//! Bidirectional RPC over Windows Named Pipes.

mod activity;
pub use activity::RequestLease;
pub mod client;
pub mod server;
pub(crate) mod wire;

pub use client::RpcClient;
pub use server::{RpcConnection, RpcServer};

/// Return the current user/logon pipe used by the tip/server pair.
/// Panics if process identity cannot be established; never falls back to a shared name.
pub fn default_pipe_name() -> String {
    try_default_pipe_name().expect("cannot establish RPC pipe identity")
}

pub fn try_default_pipe_name() -> std::io::Result<String> {
    crate::platform::RuntimeIdentity::current()?.pipe_name("server")
}

/// Return the current user/logon pipe used by the server/renderer pair.
/// Panics if process identity cannot be established.
pub fn default_renderer_pipe_name() -> String {
    try_default_renderer_pipe_name().expect("cannot establish renderer pipe identity")
}

pub fn try_default_renderer_pipe_name() -> std::io::Result<String> {
    crate::platform::RuntimeIdentity::current()?.pipe_name("renderer")
}

pub fn try_default_broker_pipe_name() -> std::io::Result<String> {
    crate::platform::RuntimeIdentity::current()?.pipe_name("broker")
}

#[derive(Debug)]
pub enum RpcError {
    Io(std::io::Error),
    Encode(prost::EncodeError),
    Decode(prost::DecodeError),
    FrameTooLarge(usize),
    Disconnected,
    UnexpectedResponse,
    Timeout,
    Overloaded,
    Protocol(String),
    Remote {
        code: crate::message::FailureCode,
        message: String,
    },
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "RPC I/O error: {error}"),
            Self::Encode(error) => write!(f, "RPC encode error: {error}"),
            Self::Decode(error) => write!(f, "RPC decode error: {error}"),
            Self::FrameTooLarge(size) => write!(f, "RPC frame is too large: {size} bytes"),
            Self::Disconnected => f.write_str("RPC connection disconnected"),
            Self::UnexpectedResponse => f.write_str("RPC response has an unexpected payload"),
            Self::Timeout => f.write_str("RPC deadline exceeded"),
            Self::Overloaded => f.write_str("RPC queue capacity exceeded"),
            Self::Protocol(message) => write!(f, "RPC protocol error: {message}"),
            Self::Remote { code, message } => write!(f, "RPC remote error ({code:?}): {message}"),
        }
    }
}

impl std::error::Error for RpcError {}

impl From<std::io::Error> for RpcError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<prost::EncodeError> for RpcError {
    fn from(error: prost::EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl From<prost::DecodeError> for RpcError {
    fn from(error: prost::DecodeError) -> Self {
        Self::Decode(error)
    }
}

pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, RpcError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    Ok(crate::framing::read(reader).await?)
}

pub(crate) async fn write_frame<W>(
    writer: &mut W,
    envelope: &crate::message::Envelope,
) -> Result<(), RpcError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    Ok(crate::framing::write(writer, &wire::encode(envelope)?).await?)
}
