//! Video codecs.
//!
//! H.264 is the initial codec (see ADR-0003): universally hardware accelerated on
//! NVIDIA, Intel and AMD, decodable by every WebCodecs/Media Foundation decoder, and
//! available through the operating system's licensed Media Foundation components, so
//! the product does not redistribute an encoder implementation.
//!
//! Bitstreams are Annex B (start codes) with SPS/PPS repeated on every keyframe so a
//! decoder can join at any keyframe.

#[cfg(windows)]
pub mod windows;

/// Encoder parameters. Changing `width`/`height` requires a new encoder; bitrate and
/// frame rate can change on the fly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Annex B bitstream.
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// Time spent in the encoder (submit → output), microseconds.
    pub encode_us: u32,
    /// Presentation timestamp supplied with the input, microseconds.
    pub pts_us: u64,
}

/// Encoder selection constraints (diagnostics, driver workarounds, benchmarks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderPreference {
    pub allow_hardware: bool,
    /// Only consider encoders whose name contains this text (case-insensitive).
    pub name_contains: Option<String>,
}

impl Default for EncoderPreference {
    fn default() -> Self {
        Self {
            allow_hardware: true,
            name_contains: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    /// Friendly encoder name, e.g. "NVIDIA H.264 Encoder MFT".
    pub name: String,
    pub hardware: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("no suitable encoder found")]
    NoEncoder,
    #[error("unsupported configuration: {0}")]
    Unsupported(String),
    #[error("encoder stalled")]
    Stalled,
    #[error("platform error: {0}")]
    Platform(String),
}

#[cfg(windows)]
impl From<::windows::core::Error> for CodecError {
    fn from(e: ::windows::core::Error) -> Self {
        CodecError::Platform(format!("{} ({:#x})", e.message(), e.code().0))
    }
}

/// Rounds a dimension down to the even value required by 4:2:0 formats.
pub fn even(v: u32) -> u32 {
    v & !1
}

/// Scales `(w, h)` so that it fits within `max_pixels` while preserving the aspect
/// ratio; the result is always even and never larger than the input.
pub fn fit_pixels(w: u32, h: u32, max_pixels: u64) -> (u32, u32) {
    let pixels = u64::from(w) * u64::from(h);
    if pixels <= max_pixels {
        return (even(w), even(h));
    }
    let scale = (max_pixels as f64 / pixels as f64).sqrt();
    (
        even((f64::from(w) * scale) as u32).max(2),
        even((f64::from(h) * scale) as u32).max(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_parity() {
        assert_eq!(fit_pixels(1920, 1080, 1920 * 1080), (1920, 1080));
        assert_eq!(fit_pixels(1921, 1081, u64::MAX), (1920, 1080));
        let (w, h) = fit_pixels(2560, 1440, 1280 * 720);
        assert_eq!((w, h), (1280, 720));
        let (w, h) = fit_pixels(3840, 2160, 1920 * 1080);
        assert_eq!((w, h), (1920, 1080));
        let (w, h) = fit_pixels(1366, 768, 640 * 360);
        assert!(w % 2 == 0 && h % 2 == 0 && u64::from(w) * u64::from(h) <= 640 * 360);
    }
}
