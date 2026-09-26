//! 内部 [`m::Envelope`] 与 protobuf [`m::RpcFrame`] 之间的消息映射。
//!
//! 本文件只维护 request、response、event 三类消息的方向和字段映射。字段边界检查位于
//! `validation`，连接开头的 hello 交换位于 `handshake`；protobuf 自身负责字节序列化。
use super::{RpcError, limits::protocol};
use crate::message::{self as m, envelope::Payload as P};
use prost::Message;

mod handshake;
mod validation;

pub(super) use handshake::{read as read_hello, write as write_hello};
use validation::{context, input_state, interaction, notification, token};

/// 当前 RPC 管道协议版本。
///
/// 仅当 Protobuf RpcFrame 的跨进程兼容约定发生不兼容变化时递增；参与通信的
/// 进程需使用相同版本。
pub const VERSION: u32 = 3;
fn invalid(text: &str) -> RpcError {
    RpcError::Protocol(text.into())
}

/// 解码 Protobuf RpcFrame，并拒绝版本不匹配或缺少帧体的输入。
pub fn decode(bytes: &[u8]) -> Result<m::RpcFrame, RpcError> {
    let frame = m::RpcFrame::decode(bytes)?;
    if frame.protocol_version != VERSION {
        return Err(invalid("incompatible protocol version"));
    }
    if frame.body.is_none() {
        return Err(invalid("missing frame body"));
    }
    Ok(frame)
}

/// 在客户端排队前校验请求载荷。
///
/// 这项检查不分配请求编号，也不构造临时信封；其规则与 [`pack`] 对请求方向的约束一致。
pub(super) fn validate_request(payload: &P) -> Result<(), RpcError> {
    match payload {
        P::IdentifyService(_) | P::QueryConfig(_) | P::Ping(_) | P::Shutdown(_) => Ok(()),
        P::UserNotification(value) => notification(value),
        P::OpenInput(value) => token(&value.token),
        P::KeyEvent(value) => token(&value.token),
        P::ContextCommand(value) => context(value),
        _ => Err(invalid("payload is not valid for a request")),
    }
}

/// 将内部信封映射为 Protobuf RpcFrame，并检查消息类型与编号是否匹配。
///
/// 此处也执行通知、输入上下文和渲染交互所需的协议约束校验；键事件必须已由
/// TIP 翻译，测试键事件不属于跨进程 RPC 管道协议。
pub fn pack(envelope: &m::Envelope) -> Result<m::RpcFrame, RpcError> {
    use m::{
        event::Notification as E, request::Operation as Q, response::Result as R,
        rpc_frame::Body as B,
    };
    let id = envelope.request_id;
    let payload = envelope
        .payload
        .clone()
        .ok_or_else(|| invalid("missing payload"))?;
    let body = match payload {
        P::IdentifyService(v) if id != protocol::EVENT_REQUEST_ID => B::Request(m::Request {
            id,
            operation: Some(Q::IdentifyService(v)),
        }),
        P::ServiceIdentity(v) if id != protocol::EVENT_REQUEST_ID => B::Response(m::Response {
            id,
            result: Some(R::ServiceIdentity(v)),
        }),
        P::UserNotification(v) if id != protocol::EVENT_REQUEST_ID => {
            notification(&v)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::UserNotification(v)),
            })
        }
        P::QueryConfig(v) if id != protocol::EVENT_REQUEST_ID => B::Request(m::Request {
            id,
            operation: Some(Q::QueryConfig(v)),
        }),
        P::ConfigValue(v) if id != protocol::EVENT_REQUEST_ID => B::Response(m::Response {
            id,
            result: Some(R::ConfigValue(v)),
        }),
        P::Ping(v) if id != protocol::EVENT_REQUEST_ID => B::Request(m::Request {
            id,
            operation: Some(Q::Ping(v)),
        }),
        P::OpenInput(v) if id != protocol::EVENT_REQUEST_ID => {
            token(&v.token)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::OpenInput(v)),
            })
        }
        P::InputOpened(v) if id != protocol::EVENT_REQUEST_ID => {
            token(&v.token)?;
            B::Response(m::Response {
                id,
                result: Some(R::InputOpened(v)),
            })
        }
        P::KeyEvent(v) if id != protocol::EVENT_REQUEST_ID => {
            token(&v.token)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::Key(v)),
            })
        }
        P::ContextCommand(v) if id != protocol::EVENT_REQUEST_ID => {
            context(&v)?;
            B::Request(m::Request {
                id,
                operation: Some(Q::Context(v)),
            })
        }
        P::Shutdown(v) if id != protocol::EVENT_REQUEST_ID => B::Request(m::Request {
            id,
            operation: Some(Q::Shutdown(v)),
        }),
        P::Pong(v) if id != protocol::EVENT_REQUEST_ID => B::Response(m::Response {
            id,
            result: Some(R::Pong(v)),
        }),
        P::ShutdownResponse(v) if id != protocol::EVENT_REQUEST_ID => B::Response(m::Response {
            id,
            result: Some(R::Shutdown(v)),
        }),
        P::Failure(v) if id != protocol::EVENT_REQUEST_ID => B::Response(m::Response {
            id,
            result: Some(R::Failure(v)),
        }),
        P::KeyEventResponse(v) => {
            let input = pack_input(v)?;
            if id == protocol::EVENT_REQUEST_ID {
                B::Event(m::Event {
                    notification: Some(E::Input(input)),
                })
            } else {
                B::Response(m::Response {
                    id,
                    result: Some(R::Input(input)),
                })
            }
        }
        P::LogEvent(v) if id == protocol::EVENT_REQUEST_ID => B::Event(m::Event {
            notification: Some(E::Diagnostic(v)),
        }),
        P::RenderSnapshot(v) if id == protocol::EVENT_REQUEST_ID => B::Event(m::Event {
            notification: Some(E::Render(v)),
        }),
        P::RendererEvent(v) if id == protocol::EVENT_REQUEST_ID => {
            interaction(&v)?;
            B::Event(m::Event {
                notification: Some(E::Interaction(v)),
            })
        }
        P::LayoutUpdate(v) if id == protocol::EVENT_REQUEST_ID => B::Event(m::Event {
            notification: Some(E::Layout(v)),
        }),
        _ => return Err(invalid("payload is not valid for request/event ID")),
    };
    Ok(m::RpcFrame {
        protocol_version: VERSION,
        body: Some(body),
    })
}

/// 将已解码的 Protobuf RpcFrame 映射回内部信封，并验证帧体及其载荷。
///
/// 此函数假定调用方已通过 [`decode`] 检查协议版本；它仍会检查请求/响应编号、
/// 必需字段和载荷约束。握手帧应由 [`read_hello`] 单独处理。
pub fn unpack(frame: m::RpcFrame) -> Result<m::Envelope, RpcError> {
    use m::{
        event::Notification as E, request::Operation as Q, response::Result as R,
        rpc_frame::Body as B,
    };
    let (id, payload) = match frame.body.ok_or_else(|| invalid("missing body"))? {
        B::Hello(_) => return Err(invalid("duplicate hello")),
        B::Request(v) => {
            if v.id == protocol::EVENT_REQUEST_ID {
                return Err(invalid("zero request ID"));
            }
            (
                v.id,
                match v.operation.ok_or_else(|| invalid("missing operation"))? {
                    Q::Ping(v) => P::Ping(v),
                    Q::IdentifyService(v) => P::IdentifyService(v),
                    Q::UserNotification(v) => {
                        notification(&v)?;
                        P::UserNotification(v)
                    }
                    Q::QueryConfig(v) => P::QueryConfig(v),
                    Q::Shutdown(v) => P::Shutdown(v),
                    Q::OpenInput(v) => {
                        token(&v.token)?;
                        P::OpenInput(v)
                    }
                    Q::Context(v) => {
                        context(&v)?;
                        P::ContextCommand(v)
                    }
                    Q::Key(v) => {
                        token(&v.token)?;
                        P::KeyEvent(v)
                    }
                },
            )
        }
        B::Response(v) => {
            if v.id == protocol::EVENT_REQUEST_ID {
                return Err(invalid("zero response ID"));
            }
            (
                v.id,
                match v.result.ok_or_else(|| invalid("missing result"))? {
                    R::Pong(v) => P::Pong(v),
                    R::ServiceIdentity(v) => P::ServiceIdentity(v),
                    R::ConfigValue(v) => P::ConfigValue(v),
                    R::Shutdown(v) => P::ShutdownResponse(v),
                    R::InputOpened(v) => {
                        token(&v.token)?;
                        P::InputOpened(v)
                    }
                    R::Input(v) => P::KeyEventResponse(unpack_input(v)?),
                    R::Failure(v) => P::Failure(v),
                },
            )
        }
        B::Event(v) => (
            protocol::EVENT_REQUEST_ID,
            match v
                .notification
                .ok_or_else(|| invalid("missing notification"))?
            {
                E::Diagnostic(v) => P::LogEvent(v),
                E::Input(v) => P::KeyEventResponse(unpack_input(v)?),
                E::Render(v) => P::RenderSnapshot(v),
                E::Layout(v) => P::LayoutUpdate(v),
                E::Interaction(v) => {
                    interaction(&v)?;
                    P::RendererEvent(v)
                }
            },
        ),
    };
    Ok(m::Envelope {
        request_id: id,
        payload: Some(payload),
    })
}

/// 将内部键事件响应打包为 RPC 输入结果，并在状态存在时校验其内容。
fn pack_input(v: m::KeyEventResponse) -> Result<m::InputResult, RpcError> {
    let state = v.state_updated.then_some(m::InputState {
        preedit: v.composition,
        cursor_utf16: v.composition_cursor,
        composing: v.composing,
        candidates: v.candidates,
        selected: v.selected_candidate,
        page_start: v.page_start,
        has_previous: v.can_page_previous,
        has_next: v.can_page_next,
        raw_input: v.raw_input,
    });
    if let Some(state) = &state {
        input_state(state)?;
    }
    Ok(m::InputResult {
        token: v.token,
        revision: v.revision,
        handled: v.eaten,
        state,
        effects: (!v.commit_text.is_empty() || v.open_emoji_panel).then_some(m::EditEffects {
            commit: v.commit_text,
            open_emoji_panel: v.open_emoji_panel,
        }),
        ascii_mode: v.ascii_mode,
        allow_rime_in_secure_fields: v.allow_rime_in_secure_fields,
        mode_indicator_request_id: v.mode_indicator_request_id,
    })
}
/// 将 RPC 输入结果还原为内部键事件响应，并校验其中的输入状态。
fn unpack_input(v: m::InputResult) -> Result<m::KeyEventResponse, RpcError> {
    let mut result = m::KeyEventResponse {
        token: v.token,
        revision: v.revision,
        eaten: v.handled,
        ascii_mode: v.ascii_mode,
        allow_rime_in_secure_fields: v.allow_rime_in_secure_fields,
        mode_indicator_request_id: v.mode_indicator_request_id,
        ..Default::default()
    };
    if let Some(s) = v.state {
        input_state(&s)?;
        result.state_updated = true;
        result.composition = s.preedit;
        result.raw_input = s.raw_input;
        result.composition_cursor = s.cursor_utf16;
        result.composing = s.composing;
        result.candidates = s.candidates;
        result.selected_candidate = s.selected;
        result.page_start = s.page_start;
        result.can_page_previous = s.has_previous;
        result.can_page_next = s.has_next;
    }
    if let Some(e) = v.effects {
        result.commit_text = e.commit;
        result.open_emoji_panel = e.open_emoji_panel;
    }
    Ok(result)
}

#[cfg(test)]
#[path = "tests/codec.rs"]
mod tests;
