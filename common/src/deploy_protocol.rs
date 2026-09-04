//! Length-prefixed protobuf telemetry for deployment UI processes.
pub use crate::message::{DeployComplete, DeployEvent, DeployLog, deploy_event::Payload};
use prost::Message;
use std::io::{self, Write};
use tokio::io::AsyncRead;

pub fn write_event(output: &mut impl Write, event: &DeployEvent) -> io::Result<()> {
    output.write_all(&crate::framing::encode(event)?)?;
    output.flush()
}

pub async fn read_event(input: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<DeployEvent>> {
    let Some(bytes) = crate::framing::read(input).await? else {
        return Ok(None);
    };
    let event = DeployEvent::decode(bytes.as_slice())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if event.payload.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty deployment event",
        ));
    }
    Ok(Some(event))
}

/// Return at Complete, not EOF: the UI is intentionally still alive.
pub async fn wait_for_completion(
    input: &mut (impl AsyncRead + Unpin),
    mut log: impl FnMut(&DeployLog) -> io::Result<()>,
) -> io::Result<DeployComplete> {
    loop {
        match read_event(input).await?.and_then(|event| event.payload) {
            Some(Payload::Log(chunk)) => log(&chunk)?,
            Some(Payload::Complete(done)) => return Ok(done),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "deployment UI exited before completion",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_matches_shared_framing_and_rejects_invalid_sizes_without_writing() {
        let mut event = DeployEvent {
            payload: Some(Payload::Log(DeployLog {
                stream: "stdout".into(),
                text: "hello".into(),
            })),
        };
        let mut bytes = Vec::new();
        write_event(&mut bytes, &event).unwrap();
        assert_eq!(bytes, crate::framing::encode(&event).unwrap());
        bytes.clear();
        assert_eq!(
            write_event(&mut bytes, &DeployEvent::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(bytes.is_empty());
        event.payload = Some(Payload::Log(DeployLog {
            stream: String::new(),
            text: "x".repeat(crate::framing::MAX_FRAME_SIZE),
        }));
        assert_eq!(
            write_event(&mut bytes, &event).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn malformed_payloads_and_truncated_frames_are_rejected() {
        for bytes in [
            vec![0, 0, 0, 0],
            vec![1, 0, 0, 0, 0xff],
            vec![2, 0, 0, 0, 0x78, 0x01],
        ] {
            assert_eq!(
                read_event(&mut bytes.as_slice()).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for bytes in [vec![1], vec![1, 0], vec![1, 0, 0], vec![2, 0, 0, 0, 0x0a]] {
            assert_eq!(
                read_event(&mut bytes.as_slice()).await.unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
    }

    #[tokio::test]
    async fn completion_leaves_following_frames_unread_and_log_errors_propagate() {
        let log = DeployEvent {
            payload: Some(Payload::Log(DeployLog::default())),
        };
        let done = DeployEvent {
            payload: Some(Payload::Complete(DeployComplete {
                success: true,
                ..Default::default()
            })),
        };
        let mut bytes = Vec::new();
        write_event(&mut bytes, &log).unwrap();
        write_event(&mut bytes, &done).unwrap();
        write_event(&mut bytes, &log).unwrap();
        let mut input = bytes.as_slice();
        let mut logs = 0;
        assert!(
            wait_for_completion(&mut input, |_| {
                logs += 1;
                Ok(())
            })
            .await
            .unwrap()
            .success
        );
        assert_eq!(logs, 1);
        assert_eq!(read_event(&mut input).await.unwrap(), Some(log));
        assert_eq!(
            wait_for_completion(&mut bytes.as_slice(), |_| Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "log sink closed"
            )))
            .await
            .unwrap_err()
            .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn writer_flushes_and_propagates_sink_errors() {
        struct Sink {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.push(bytes[0]); // Exercise write_all with short writes.
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "flush failed"))
            }
        }
        let event = DeployEvent {
            payload: Some(Payload::Log(DeployLog::default())),
        };
        let mut sink = Sink {
            bytes: Vec::new(),
            flushes: 0,
        };
        assert_eq!(
            write_event(&mut sink, &event).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(sink.bytes, crate::framing::encode(&event).unwrap());
        assert_eq!(sink.flushes, 1);
    }
    #[tokio::test]
    async fn log_and_result_round_trip() {
        let log = DeployEvent {
            payload: Some(Payload::Log(DeployLog {
                stream: "stderr".into(),
                text: "中文\ncomplete\r\n".repeat(1000),
            })),
        };
        let done = DeployEvent {
            payload: Some(Payload::Complete(DeployComplete {
                success: false,
                exit_code: Some(1),
                message: "失败".into(),
            })),
        };
        let mut bytes = Vec::new();
        write_event(&mut bytes, &log).unwrap();
        write_event(&mut bytes, &done).unwrap();
        let mut input = bytes.as_slice();
        assert_eq!(read_event(&mut input).await.unwrap(), Some(log));
        assert_eq!(read_event(&mut input).await.unwrap(), Some(done));
        assert_eq!(read_event(&mut input).await.unwrap(), None);
        assert!(read_event(&mut &bytes[..2]).await.is_err());
        assert!(
            read_event(&mut &(u32::MAX.to_le_bytes())[..])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn completion_does_not_wait_for_ui_exit() {
        use tokio::io::AsyncWriteExt;
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        let event = DeployEvent {
            payload: Some(Payload::Complete(DeployComplete {
                success: true,
                exit_code: Some(0),
                message: "完成".into(),
            })),
        };
        let mut bytes = Vec::new();
        write_event(&mut bytes, &event).unwrap();
        // Deliberately fragment the frame and retain the writer after completion.
        let task = tokio::spawn(async move {
            for byte in bytes {
                writer.write_all(&[byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
            writer
        });
        let done = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            wait_for_completion(&mut reader, |_| Ok(())),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(done.success);
        drop(task.await.unwrap());
        assert!(wait_for_completion(&mut &[][..], |_| Ok(())).await.is_err());
    }
}
