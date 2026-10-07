//! Aionix Connect data-plane protocol.
//!
//! The schema lives in `proto/remote.proto`; this crate exposes the generated types
//! ([`pb`]) plus bounded, length-delimited framing helpers ([`framing`]).

pub mod framing;

/// Generated protobuf types.
#[allow(clippy::all, missing_docs)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/aionix.remote.v1.rs"));
}

/// QUIC ALPN identifying this protocol version. Bump only for breaking changes.
pub const ALPN: &[u8] = b"aionix/remote/1";

/// Protocol revision inside the ALPN (additive changes only).
pub const PROTOCOL_VERSION: u32 = 1;

/// Size limits enforced by every reader. Peers sending larger messages are
/// disconnected with a protocol error.
pub mod limits {
    /// Control messages (largest legitimate one is a cursor shape or clipboard text).
    pub const MAX_CONTROL_MESSAGE: usize = 2 * 1024 * 1024;
    /// Clipboard text in bytes (UTF-8).
    pub const MAX_CLIPBOARD_TEXT: usize = 1024 * 1024;
    /// Input batches.
    pub const MAX_INPUT_BATCH: usize = 64 * 1024;
    /// Video frame header.
    pub const MAX_FRAME_HEADER: usize = 256;
    /// One encoded video frame (a 4K keyframe at very high quality stays well below).
    pub const MAX_FRAME_PAYLOAD: usize = 16 * 1024 * 1024;
    /// Cursor shapes larger than this are rejected (256x256 RGBA).
    pub const MAX_CURSOR_BYTES: usize = 256 * 256 * 4;
}
