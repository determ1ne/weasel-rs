//! 基于 Windows 命名管道的双向 RPC。
//!
//! 模块提供客户端与服务端连接、协议错误类型，以及按长度前缀传输编码信封的数据帧辅助函数。
//! 管道访问控制由 Windows 安全描述符负责；协议角色只约束握手后的消息类型，不用于认证。

mod activity;
pub use activity::{RequestLease, RetireCandidate};
pub mod client;
pub(crate) mod codec;
mod limits;
pub mod server;

pub use client::RpcClient;
/// 当前 RPC 管道协议版本。
///
/// 该值来自编解码层的协议常量，可供握手或兼容性检查使用。
pub use codec::VERSION as PROTOCOL_VERSION;
pub use server::{RpcConnection, RpcServer};

/// 返回当前用户登录会话中 TIP 与服务端共用的管道名。
///
/// 若无法建立运行时身份则 panic；不会退回到所有用户共享的固定名称。
pub fn default_pipe_name() -> String {
    try_default_pipe_name().expect("cannot establish RPC pipe identity")
}

/// 尝试返回当前用户登录会话中 TIP 与服务端共用的管道名。
///
/// 返回新分配的字符串；运行时身份不可用时以 [`std::io::Error`] 返回错误，不会使用共享名称。
pub fn try_default_pipe_name() -> std::io::Result<String> {
    crate::windows_security::RuntimeIdentity::current()?.pipe_name("server")
}

/// 返回当前用户登录会话中服务端与渲染器共用的管道名。
///
/// 若无法建立运行时身份则 panic；不会退回到共享名称。
pub fn default_renderer_pipe_name() -> String {
    try_default_renderer_pipe_name().expect("cannot establish renderer pipe identity")
}

/// 尝试返回当前用户登录会话中服务端与渲染器共用的管道名。
///
/// 返回新分配的字符串；身份或管道名构造失败时以 [`std::io::Error`] 返回。
pub fn try_default_renderer_pipe_name() -> std::io::Result<String> {
    crate::windows_security::RuntimeIdentity::current()?.pipe_name("renderer")
}

/// 尝试返回当前用户登录会话中各组件共用的 Broker 管道名。
///
/// 返回新分配的字符串；身份或管道名构造失败时以 [`std::io::Error`] 返回。
pub fn try_default_broker_pipe_name() -> std::io::Result<String> {
    crate::windows_security::RuntimeIdentity::current()?.pipe_name("broker")
}

/// RPC 操作可能产生的传输、编解码、协议及远端业务错误。
///
/// 此类型统一包装本地命名管道 I/O、Prost 编解码、帧大小限制、断开、超时、队列过载、
/// 协议违规和远端返回的失败信息。`Remote` 携带远端失败代码与消息；其余变体描述本地
/// 传输或协议处理过程。实现 [`std::error::Error`]，可作为标准错误链中的来源。
#[derive(Debug)]
pub enum RpcError {
    /// Windows 管道或其他底层异步 I/O 操作失败。
    Io(std::io::Error),
    /// 信封编码为 Protobuf 字节时失败。
    Encode(prost::EncodeError),
    /// 从 Protobuf 字节解码信封时失败。
    Decode(prost::DecodeError),
    /// 数据帧长度超过传输层允许上限；字段为报告的帧字节数。
    FrameTooLarge(usize),
    /// 连接已断开、关闭，或相关接收端已被丢弃。
    Disconnected,
    /// 收到的响应载荷与调用方预期类型不符。
    UnexpectedResponse,
    /// 操作超过其内部规定的等待期限。
    Timeout,
    /// 有界发送队列已满，无法接纳新消息。
    Overloaded,
    /// 握手、对端角色、消息类型或帧内容违反 RPC 协议。
    Protocol(String),
    /// 对端返回的业务失败代码及说明文字。
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

/// 读取一个长度前缀数据帧。
///
/// 完整帧以拥有字节缓冲区的 `Some` 返回；在帧开始前遇到 EOF 时返回 `None`。不完整帧、
/// 超限帧或底层读取失败返回 [`RpcError`]。读取器由可变借用保留给调用方，且必须支持
/// Tokio 异步读取并实现 `Unpin`。
pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, RpcError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    Ok(crate::data_frame::read(reader).await?)
}

/// 将信封编码并写为一个长度前缀数据帧。
///
/// 信封仅被借用；编码得到的字节写入器后释放。编码失败、帧超限或底层写入失败均返回
/// [`RpcError`]。写入器必须支持 Tokio 异步写入并实现 `Unpin`；成功表示帧已交给底层写入，
/// 不表示对端已读取或处理。
pub(crate) async fn write_frame<W>(
    writer: &mut W,
    envelope: &crate::message::Envelope,
) -> Result<(), RpcError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    Ok(crate::data_frame::write(writer, &codec::pack(envelope)?).await?)
}
