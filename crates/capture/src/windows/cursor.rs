//! Conversion of DXGI pointer shapes to straight RGBA.

use crate::{CursorShape, fnv1a64};

pub(crate) const SHAPE_MONOCHROME: u32 = 1;
pub(crate) const SHAPE_COLOR: u32 = 2;
pub(crate) const SHAPE_MASKED_COLOR: u32 = 4;

/// Converts a raw pointer shape buffer. `height` for monochrome shapes is the DXGI
/// value (AND mask + XOR mask stacked, i.e. twice the visible height).
pub(crate) fn convert_shape(
    kind: u32,
    width: u32,
    height: u32,
    pitch: u32,
    hotspot: (u32, u32),
    buf: &[u8],
) -> Option<CursorShape> {
    let (w, h, rgba) = match kind {
        SHAPE_COLOR => (width, height, color(width, height, pitch, buf)?),
        SHAPE_MASKED_COLOR => (width, height, masked_color(width, height, pitch, buf)?),
        SHAPE_MONOCHROME => (
            width,
            height / 2,
            monochrome(width, height / 2, pitch, buf)?,
        ),
        _ => return None,
    };
    if w == 0 || h == 0 {
        return None;
    }
    let mut id_input = Vec::with_capacity(rgba.len() + 16);
    id_input.extend_from_slice(&w.to_le_bytes());
    id_input.extend_from_slice(&h.to_le_bytes());
    id_input.extend_from_slice(&hotspot.0.to_le_bytes());
    id_input.extend_from_slice(&hotspot.1.to_le_bytes());
    id_input.extend_from_slice(&rgba);
    Some(CursorShape {
        id: fnv1a64(&id_input),
        width: w,
        height: h,
        hotspot_x: hotspot.0,
        hotspot_y: hotspot.1,
        rgba,
    })
}

fn row(buf: &[u8], pitch: u32, y: u32, len: usize) -> Option<&[u8]> {
    let start = (y * pitch) as usize;
    buf.get(start..start + len)
}

/// 32bpp BGRA with valid alpha.
fn color(w: u32, h: u32, pitch: u32, buf: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for px in row(buf, pitch, y, (w * 4) as usize)?.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Some(out)
}

/// 32bpp BGRA where the alpha byte is a mask: 0 = replace pixel, 0xFF = XOR with the
/// screen. XOR pixels cannot be reproduced without the background; black XOR pixels are
/// no-ops (transparent) and others are drawn opaque, which matches how they look on
/// typical backgrounds.
fn masked_color(w: u32, h: u32, pitch: u32, buf: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for px in row(buf, pitch, y, (w * 4) as usize)?.as_chunks::<4>().0 {
            let (b, g, r, mask) = (px[0], px[1], px[2], px[3]);
            if mask == 0 {
                out.extend_from_slice(&[r, g, b, 255]);
            } else if r == 0 && g == 0 && b == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }
    Some(out)
}

/// 1bpp AND mask followed by 1bpp XOR mask.
///
/// | AND | XOR | meaning     | rendered as                     |
/// |-----|-----|-------------|---------------------------------|
/// | 0   | 0   | black       | black                           |
/// | 0   | 1   | white       | white                           |
/// | 1   | 0   | transparent | transparent                     |
/// | 1   | 1   | invert      | black, with a white outline     |
///
/// The outline keeps inverting cursors (I-beam) visible on dark and light backgrounds.
fn monochrome(w: u32, h: u32, pitch: u32, buf: &[u8]) -> Option<Vec<u8>> {
    let bit = |plane: u32, x: u32, y: u32| -> Option<bool> {
        let byte = *buf.get(((plane * h + y) * pitch + x / 8) as usize)?;
        Some(byte & (0x80 >> (x % 8)) != 0)
    };
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![0u8; wu * hu * 4];
    let mut invert = vec![false; wu * hu];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let px = &mut out[i * 4..i * 4 + 4];
            match (bit(0, x, y)?, bit(1, x, y)?) {
                (false, false) => px.copy_from_slice(&[0, 0, 0, 255]),
                (false, true) => px.copy_from_slice(&[255, 255, 255, 255]),
                (true, false) => {}
                (true, true) => {
                    px.copy_from_slice(&[0, 0, 0, 255]);
                    invert[i] = true;
                }
            }
        }
    }
    super::cursor_poll::outline_inverted(&mut out, &invert, wu, hu);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_swaps_channels() {
        let buf = [1u8, 2, 3, 4];
        let s = convert_shape(SHAPE_COLOR, 1, 1, 4, (0, 0), &buf).unwrap();
        assert_eq!(s.rgba, vec![3, 2, 1, 4]);
    }

    #[test]
    fn monochrome_decodes_all_four_states_and_outlines_invert() {
        // 8x2 visible cursor: pitch 1 byte, AND plane then XOR plane.
        // Row 0: pixels 0..4 = black, white, transparent, invert, rest transparent.
        let and = [0b0011_1111u8, 0xFF];
        let xor = [0b0101_0000u8, 0x00];
        let buf = [and[0], and[1], xor[0], xor[1]];
        let s = convert_shape(SHAPE_MONOCHROME, 8, 4, 1, (0, 0), &buf).unwrap();
        assert_eq!((s.width, s.height), (8, 2));
        let px = |x: usize, y: usize| &s.rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
        assert_eq!(px(0, 0), &[0, 0, 0, 255]);
        assert_eq!(px(1, 0), &[255, 255, 255, 255]);
        assert_eq!(px(3, 0), &[0, 0, 0, 255]);
        // Neighbours of the invert pixel get a white outline.
        assert_eq!(px(4, 0), &[255, 255, 255, 255]);
        assert_eq!(px(3, 1), &[255, 255, 255, 255]);
        // Far transparent pixel stays transparent.
        assert_eq!(px(7, 1), &[0, 0, 0, 0]);
    }

    #[test]
    fn truncated_buffers_are_rejected() {
        assert!(convert_shape(SHAPE_COLOR, 4, 4, 16, (0, 0), &[0u8; 10]).is_none());
    }

    #[test]
    fn identical_shapes_share_ids() {
        let buf = [9u8; 16];
        let a = convert_shape(SHAPE_COLOR, 2, 2, 8, (1, 1), &buf).unwrap();
        let b = convert_shape(SHAPE_COLOR, 2, 2, 8, (1, 1), &buf).unwrap();
        let c = convert_shape(SHAPE_COLOR, 2, 2, 8, (0, 1), &buf).unwrap();
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
    }
}
