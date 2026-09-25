//! Protobuf 消息共用的长度前缀数据帧格式。
//!
//! 帧头是 4 字节小端序长度，后接 Protobuf 消息体。读取操作会完整消费一帧；取消尚未完成的
//! 读取会丢弃该次 future 持有的部分帧，因此流读取应放在专用 reader 任务中，避免取消后
//! 从半帧位置继续解析。
use prost::Message;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 单个 Protobuf 消息体允许的最大长度，单位为字节；不包含 4 字节帧头。
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// 从异步流读取一帧，并返回不含长度前缀的消息体。
///
/// 仅在帧边界遇到 EOF 时返回 `Ok(None)`。零长度、超过 [`MAX_FRAME_SIZE`] 的帧头以及不完整帧
/// 均作为错误处理；在分配消息体缓冲区前会先验证长度。
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    if reader.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await?;
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid protobuf frame size",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(Some(bytes))
}

/// 将 Protobuf 消息编码为完整帧，包括小端序长度前缀。
///
/// 消息体必须非空且不超过 [`MAX_FRAME_SIZE`]。编码失败或长度不合法时返回错误。
pub fn encode(message: &impl Message) -> io::Result<Vec<u8>> {
    let size = message.encoded_len();
    if size == 0 || size > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid protobuf frame size",
        ));
    }
    let mut bytes = Vec::with_capacity(size + 4);
    bytes.extend_from_slice(&(size as u32).to_le_bytes());
    message.encode(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes)
}

/// 将一条 Protobuf 消息作为完整帧写入异步流并刷新。
///
/// 编码、写入或刷新错误会向调用方传播。调用期间由调用方独占可变写入器。
pub async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &impl Message,
) -> io::Result<()> {
    writer.write_all(&encode(message)?).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn eof_is_only_valid_between_frames() {
        assert!(read(&mut &[][..]).await.unwrap().is_none());
        for prefix in 1..4 {
            assert_eq!(
                read(&mut &[3, 0, 0, 0][..prefix]).await.unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
        assert_eq!(
            read(&mut &[3, 0, 0, 0, 1, 2][..]).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn rejects_zero_and_oversized_before_allocation() {
        for size in [0_u32, MAX_FRAME_SIZE as u32 + 1, u32::MAX] {
            assert_eq!(
                read(&mut &size.to_le_bytes()[..]).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[tokio::test]
    async fn fragmented_and_concatenated_frames() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        let task = tokio::spawn(async move {
            for byte in [3, 0, 0, 0, 1, 2, 3, 1, 0, 0, 0, 9] {
                writer.write_all(&[byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        assert_eq!(read(&mut reader).await.unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(read(&mut reader).await.unwrap(), Some(vec![9]));
        assert_eq!(read(&mut reader).await.unwrap(), None);
        task.await.unwrap();
    }
}
