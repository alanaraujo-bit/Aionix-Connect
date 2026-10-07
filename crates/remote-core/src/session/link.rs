//! Wire plumbing shared by both coordinators: control stream reader/writer tasks, the
//! per-frame video stream format and connection close codes.
//!
//! Framed reads are not cancellation safe, so each stream is read by its own task that
//! forwards decoded messages (or the loss of the link) over a channel. Events carry the
//! link generation so that, after a resume, late events of the old connection are
//! recognised and ignored.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use aionix_protocol::{
    framing::{FrameError, encode_frame, read_frame, write_frame},
    limits, pb,
};
use aionix_transport::{RecvStream, SendStream};
use prost::Message;
use tokio::{sync::mpsc, task::JoinHandle};

use super::media::EncodedVideoFrame;

/// Application close codes.
pub(crate) const CLOSE_NORMAL: u32 = 0;
pub(crate) const CLOSE_REJECTED: u32 = 1;
pub(crate) const CLOSE_PROTOCOL: u32 = 2;
/// Fault injection: emulates the network dropping the connection.
pub(crate) const CLOSE_FAULT: u32 = 3;
/// Replaced by a resumed connection.
pub(crate) const CLOSE_SUPERSEDED: u32 = 4;

/// Time allowed for the last control messages (Goodbye) to be acknowledged.
pub(crate) const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(crate) enum LinkEvent<M> {
    Message(u64, M),
    Lost(u64, String),
}

/// Reads length-delimited `M` messages until the stream ends or fails.
pub(crate) fn spawn_reader<M>(
    generation: u64,
    mut recv: RecvStream,
    tx: mpsc::Sender<LinkEvent<M>>,
) -> JoinHandle<()>
where
    M: Message + Default + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match read_frame::<_, M>(&mut recv, limits::MAX_CONTROL_MESSAGE).await {
                Ok(m) => {
                    if tx.send(LinkEvent::Message(generation, m)).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let reason = match e {
                        FrameError::Closed => "control stream closed".to_owned(),
                        other => other.to_string(),
                    };
                    let _ = tx.send(LinkEvent::Lost(generation, reason)).await;
                    return;
                }
            }
        }
    })
}

/// Serialized writer for one control stream.
pub(crate) struct ControlWriter<M> {
    tx: mpsc::Sender<M>,
    task: JoinHandle<()>,
}

impl<M: Message + Send + 'static> ControlWriter<M> {
    pub fn spawn(mut send: SendStream) -> Self {
        let (tx, mut rx) = mpsc::channel::<M>(256);
        let task = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if write_frame(&mut send, &m).await.is_err() {
                    return;
                }
            }
            // Orderly close: deliver what was written before the connection goes.
            if send.finish().is_ok() {
                let _ = tokio::time::timeout(FLUSH_TIMEOUT, send.acknowledged()).await;
            }
        });
        Self { tx, task }
    }

    /// Queues `m`; waits if the queue is full. Returns false if the link is gone.
    pub async fn send(&self, m: M) -> bool {
        self.tx.send(m).await.is_ok()
    }

    /// Queues `m` unless the queue is full (for messages that are superseded quickly,
    /// such as cursor positions).
    pub fn try_send(&self, m: M) -> bool {
        self.tx.try_send(m).is_ok()
    }

    /// Flushes queued messages and finishes the stream (bounded).
    pub async fn close(self) {
        drop(self.tx);
        let _ = tokio::time::timeout(FLUSH_TIMEOUT * 2, self.task).await;
    }
}

/// Encoded frames sent but not yet acknowledged by the peer.
#[derive(Debug, Default)]
pub(crate) struct InFlight {
    frames: Mutex<BTreeMap<u64, Instant>>,
    count: Arc<AtomicU32>,
}

impl InFlight {
    /// Shared counter read by the media thread for backpressure.
    pub fn counter(&self) -> Arc<AtomicU32> {
        self.count.clone()
    }

    pub fn begin(&self, seq: u64) {
        let mut f = self.frames.lock().unwrap();
        f.insert(seq, Instant::now());
        self.count.store(f.len() as u32, Ordering::Relaxed);
    }

    pub fn end(&self, seq: u64) {
        let mut f = self.frames.lock().unwrap();
        f.remove(&seq);
        self.count.store(f.len() as u32, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        let mut f = self.frames.lock().unwrap();
        f.clear();
        self.count.store(0, Ordering::Relaxed);
    }

    pub fn len(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn oldest_age(&self) -> Duration {
        self.frames
            .lock()
            .unwrap()
            .values()
            .min()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }
}

/// One video frame per unidirectional stream: framed header, then the bitstream.
pub(crate) fn encode_video_frame(f: &EncodedVideoFrame) -> Vec<u8> {
    let header = pb::VideoFrameHeader {
        stream_epoch: f.epoch,
        seq: f.seq,
        keyframe: f.keyframe,
        capture_time_us: f.capture_time_us,
        encode_us: f.encode_us,
        payload_len: f.data.len() as u32,
    };
    let mut buf = encode_frame(&header);
    buf.extend_from_slice(&f.data);
    buf
}

pub(crate) fn decode_video_frame(mut bytes: Vec<u8>) -> Result<EncodedVideoFrame, String> {
    if bytes.len() < 4 {
        return Err("truncated video frame".into());
    }
    let header_len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if header_len > limits::MAX_FRAME_HEADER || bytes.len() < 4 + header_len {
        return Err("invalid video frame header length".into());
    }
    let header = pb::VideoFrameHeader::decode(&bytes[4..4 + header_len])
        .map_err(|e| format!("invalid video frame header: {e}"))?;
    let payload = bytes.split_off(4 + header_len);
    if payload.len() != header.payload_len as usize {
        return Err("video payload length mismatch".into());
    }
    Ok(EncodedVideoFrame {
        epoch: header.stream_epoch,
        seq: header.seq,
        keyframe: header.keyframe,
        capture_time_us: header.capture_time_us,
        encode_us: header.encode_us,
        data: payload,
    })
}

/// Largest acceptable video stream (framed header + payload).
pub(crate) const MAX_VIDEO_STREAM: usize = 4 + limits::MAX_FRAME_HEADER + limits::MAX_FRAME_PAYLOAD;

pub(crate) fn host_msg(msg: pb::host_to_viewer::Msg) -> pb::HostToViewer {
    pb::HostToViewer { msg: Some(msg) }
}

pub(crate) fn viewer_msg(msg: pb::viewer_to_host::Msg) -> pb::ViewerToHost {
    pb::ViewerToHost { msg: Some(msg) }
}

pub(crate) fn goodbye(reason: pb::GoodbyeReason, detail: &str) -> pb::Goodbye {
    pb::Goodbye {
        reason: reason as i32,
        detail: detail.to_owned(),
    }
}

/// Writes one input batch.
pub(crate) async fn write_input(
    send: &mut SendStream,
    events: Vec<pb::InputEvent>,
) -> Result<(), FrameError> {
    write_frame(send, &pb::InputBatch { events }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_frame_roundtrip_and_validation() {
        let f = EncodedVideoFrame {
            epoch: 3,
            seq: 42,
            keyframe: true,
            capture_time_us: 123,
            encode_us: 4567,
            data: vec![0, 0, 0, 1, 0x65, 1, 2, 3],
        };
        let bytes = encode_video_frame(&f);
        let back = decode_video_frame(bytes.clone()).unwrap();
        assert_eq!((back.epoch, back.seq, back.keyframe), (3, 42, true));
        assert_eq!(back.data, f.data);
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(decode_video_frame(truncated).is_err());
        assert!(decode_video_frame(vec![0xff, 0xff, 0xff, 0x7f]).is_err());
    }

    #[test]
    fn in_flight_tracks_oldest() {
        let f = InFlight::default();
        f.begin(1);
        f.begin(2);
        assert_eq!(f.len(), 2);
        f.end(1);
        assert_eq!(f.counter().load(Ordering::Relaxed), 1);
        f.clear();
        assert_eq!(f.len(), 0);
        assert_eq!(f.oldest_age(), Duration::ZERO);
    }
}
