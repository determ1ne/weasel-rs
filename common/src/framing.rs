//! Shared u32 little-endian length-prefixed protobuf frames.
//! A reader future owns its partial frame: run it in a dedicated reader task.
use prost::Message;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

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
