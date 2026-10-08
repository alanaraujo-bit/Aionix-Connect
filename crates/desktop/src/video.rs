//! Decoded video → egui texture.
//!
//! The remote-core decode thread hands over tightly packed NV12 pictures (BT.709,
//! limited range — what the host's D3D11 video processor produces). They are converted
//! to RGBA on that same thread and only the latest picture is kept: the UI thread
//! uploads it on its next repaint, so a slow repaint drops frames instead of queueing
//! them.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use aionix_remote_core::session::media::{DecodedVideoFrame, FrameConsumer};
use egui::{Color32, ColorImage};

/// Latest decoded picture, shared between the decode thread and the UI.
pub struct FrameSlot {
    latest: Mutex<Option<ColorImage>>,
    ctx: egui::Context,
    rendered: AtomicU64,
    received: AtomicU64,
}

impl FrameSlot {
    pub fn new(ctx: egui::Context) -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(None),
            ctx,
            rendered: AtomicU64::new(0),
            received: AtomicU64::new(0),
        })
    }

    /// Takes the newest picture not shown yet.
    pub fn take(&self) -> Option<ColorImage> {
        let img = self.latest.lock().unwrap().take();
        if img.is_some() {
            self.rendered.fetch_add(1, Ordering::Relaxed);
        }
        img
    }

    /// (pictures received from the decoder, pictures handed to the renderer)
    pub fn counters(&self) -> (u64, u64) {
        (
            self.received.load(Ordering::Relaxed),
            self.rendered.load(Ordering::Relaxed),
        )
    }
}

/// `FrameConsumer` feeding a [`FrameSlot`].
pub struct VideoSink(pub Arc<FrameSlot>);

impl FrameConsumer for VideoSink {
    fn on_frame(&mut self, frame: DecodedVideoFrame) {
        let Some(image) = nv12_to_image(frame.width, frame.height, &frame.nv12) else {
            return;
        };
        self.0.received.fetch_add(1, Ordering::Relaxed);
        *self.0.latest.lock().unwrap() = Some(image);
        self.0.ctx.request_repaint();
    }
}

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Converts tightly packed NV12 (BT.709 limited range) to an opaque RGBA image.
/// Returns `None` for inconsistent sizes.
pub fn nv12_to_image(width: u32, height: u32, nv12: &[u8]) -> Option<ColorImage> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let chroma_w = w.div_ceil(2);
    let chroma_h = h.div_ceil(2);
    let y_len = w * h;
    if nv12.len() < y_len + chroma_w * 2 * chroma_h {
        return None;
    }
    let (y_plane, uv_plane) = nv12.split_at(y_len);
    let mut pixels = vec![Color32::BLACK; w * h];
    for row in 0..h {
        let y_row = &y_plane[row * w..(row + 1) * w];
        let uv_row = &uv_plane[(row / 2) * chroma_w * 2..];
        let out = &mut pixels[row * w..(row + 1) * w];
        for col in 0..w {
            let c = 298 * (i32::from(y_row[col]) - 16);
            let u = i32::from(uv_row[(col / 2) * 2]) - 128;
            let v = i32::from(uv_row[(col / 2) * 2 + 1]) - 128;
            let r = (c + 459 * v + 128) >> 8;
            let g = (c - 55 * u - 136 * v + 128) >> 8;
            let b = (c + 541 * u + 128) >> 8;
            out[col] = Color32::from_rgb(clamp8(r), clamp8(g), clamp8(b));
        }
    }
    Some(ColorImage::new([w, h], pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nv12(w: usize, h: usize, y: u8, u: u8, v: u8) -> Vec<u8> {
        let mut buf = vec![y; w * h];
        for _ in 0..(w / 2) * (h / 2) {
            buf.extend_from_slice(&[u, v]);
        }
        buf
    }

    #[test]
    fn limited_range_black_white_and_primaries() {
        let px = |y, u, v| nv12_to_image(2, 2, &nv12(2, 2, y, u, v)).unwrap().pixels[0];
        assert_eq!(px(16, 128, 128), Color32::from_rgb(0, 0, 0));
        assert_eq!(px(235, 128, 128), Color32::from_rgb(255, 255, 255));
        // BT.709 limited-range red (Y=63, Cb=102, Cr=240).
        let red = px(63, 102, 240);
        assert!(red.r() > 250 && red.g() < 6 && red.b() < 6, "{red:?}");
        // BT.709 limited-range blue (Y=32, Cb=240, Cr=118).
        let blue = px(32, 240, 118);
        assert!(blue.b() > 250 && blue.r() < 6 && blue.g() < 6, "{blue:?}");
    }

    #[test]
    fn rejects_truncated_buffers() {
        assert!(nv12_to_image(4, 4, &[0; 10]).is_none());
        assert!(nv12_to_image(0, 4, &[]).is_none());
    }
}
