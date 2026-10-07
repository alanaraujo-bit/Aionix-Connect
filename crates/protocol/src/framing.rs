//! Length-delimited protobuf framing with strict size limits.
//!
//! Each frame is a little-endian `u32` byte length followed by the encoded message.
//! A fixed-width prefix (instead of a varint) lets readers reject oversized frames
//! before allocating, with a single `read_exact`.

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {size} bytes exceeds limit of {limit} bytes")]
    TooLarge { size: usize, limit: usize },
    #[error("invalid message: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("stream closed")]
    Closed,
}

/// Encodes `msg` into a standalone frame buffer.
pub fn encode_frame<M: Message>(msg: &M) -> Vec<u8> {
    let len = msg.encoded_len();
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_le_bytes());
    msg.encode(&mut buf).expect("Vec has unlimited capacity");
    buf
}

pub async fn write_frame<W, M>(w: &mut W, msg: &M) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    M: Message,
{
    w.write_all(&encode_frame(msg)).await?;
    Ok(())
}

/// Reads one frame. Returns [`FrameError::Closed`] on a clean end of stream at a
/// frame boundary.
pub async fn read_frame<R, M>(r: &mut R, limit: usize) -> Result<M, FrameError>
where
    R: AsyncRead + Unpin,
    M: Message + Default,
{
    let buf = read_frame_bytes(r, limit).await?;
    Ok(M::decode(buf.as_slice())?)
}

pub async fn read_frame_bytes<R>(r: &mut R, limit: usize) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let size = u32::from_le_bytes(len_buf) as usize;
    if size > limit {
        return Err(FrameError::TooLarge { size, limit });
    }
    let mut buf = vec![0u8; size];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb;

    #[tokio::test]
    async fn roundtrip_control_message() {
        let msg = pb::ViewerToHost {
            msg: Some(pb::viewer_to_host::Msg::Hello(pb::Hello {
                protocol_version: crate::PROTOCOL_VERSION,
                client_version: "0.1.0".into(),
                session_ticket: "axt1.x.y.z".into(),
                capabilities: Some(pb::Capabilities {
                    decoders: vec![pb::VideoCodec::H264 as i32],
                    clipboard_text: true,
                    max_width: 3840,
                    max_height: 2160,
                    max_fps: 60,
                }),
                resume_token: String::new(),
            })),
        };
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &msg).await.unwrap();
        let got: pb::ViewerToHost = read_frame(&mut b, 4096).await.unwrap();
        assert_eq!(got, msg);
    }

    #[tokio::test]
    async fn rejects_oversized_frames_before_allocating() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(u32::MAX).to_le_bytes()).await.unwrap();
        let err = read_frame::<_, pb::Ping>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn clean_eof_is_closed_and_truncated_is_io() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(matches!(read_frame::<_, pb::Ping>(&mut b, 1024).await, Err(FrameError::Closed)));

        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_le_bytes()).await.unwrap();
        a.write_all(&[1, 2, 3]).await.unwrap();
        drop(a);
        assert!(matches!(read_frame::<_, pb::Ping>(&mut b, 1024).await, Err(FrameError::Io(_))));
    }

    #[tokio::test]
    async fn corrupted_payload_is_decode_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&4u32.to_le_bytes()).await.unwrap();
        a.write_all(&[0xff, 0xff, 0xff, 0xff]).await.unwrap();
        assert!(matches!(read_frame::<_, pb::Ping>(&mut b, 1024).await, Err(FrameError::Decode(_))));
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compat() {
        // A future version adds field 99 to Ping; current decoders must still accept it.
        let mut buf = Vec::new();
        pb::Ping { id: 7, sent_us: 9 }.encode(&mut buf).unwrap();
        prost::encoding::string::encode(99, &"future".to_string(), &mut buf);
        let ping = pb::Ping::decode(buf.as_slice()).unwrap();
        assert_eq!(ping.id, 7);
    }
}
