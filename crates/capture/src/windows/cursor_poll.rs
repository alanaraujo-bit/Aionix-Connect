//! Cursor tracking through the window manager, for backends that do not report the
//! hardware cursor (Windows.Graphics.Capture, GDI).

use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
    },
    UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HCURSOR, HICON, ICONINFO,
    },
};

use crate::{CursorPosition, CursorShape, fnv1a64};

pub(crate) struct CursorTracker {
    origin: (i32, i32),
    last_handle: Option<isize>,
    last_position: Option<CursorPosition>,
}

impl CursorTracker {
    /// `origin` is the captured display's top-left corner in desktop coordinates.
    pub fn new(origin: (i32, i32)) -> Self {
        Self { origin, last_handle: None, last_position: None }
    }

    /// Returns what changed since the previous call.
    pub fn poll(&mut self) -> (Option<CursorPosition>, Option<CursorShape>) {
        let mut info = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
        if unsafe { GetCursorInfo(&mut info) }.is_err() {
            return (None, None);
        }
        let POINT { x, y } = info.ptScreenPos;
        let pos = CursorPosition {
            x: x - self.origin.0,
            y: y - self.origin.1,
            visible: (info.flags.0 & CURSOR_SHOWING.0) != 0 && !info.hCursor.is_invalid(),
        };
        let pos_changed = self.last_position != Some(pos);
        self.last_position = Some(pos);

        let handle = info.hCursor.0 as isize;
        let shape = if pos.visible && self.last_handle != Some(handle) {
            self.last_handle = Some(handle);
            read_shape(info.hCursor)
        } else {
            None
        };
        (pos_changed.then_some(pos), shape)
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }
}

/// Reads a bitmap as top-down 32bpp BGRA.
fn bitmap_bgra(bmp: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(HGDIOBJ(bmp.0), std::mem::size_of::<BITMAP>() as i32, Some((&mut bm as *mut BITMAP).cast()))
    };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmWidth > 512 || bm.bmHeight > 1024 {
        return None;
    }
    let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let hdc = unsafe { GetDC(None) };
    let lines = unsafe { GetDIBits(hdc, bmp, 0, h, Some(buf.as_mut_ptr().cast()), &mut bi, DIB_RGB_COLORS) };
    unsafe { ReleaseDC(None, hdc) };
    (lines == h as i32).then_some((w, h, buf))
}

fn read_shape(cursor: HCURSOR) -> Option<CursorShape> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(HICON(cursor.0), &mut ii) }.ok()?;
    let mask = Bitmap(ii.hbmMask);
    let color = Bitmap(ii.hbmColor);
    let hotspot = (ii.xHotspot, ii.yHotspot);

    let (w, h, rgba) = if !color.0.is_invalid() {
        let (w, h, bgra) = bitmap_bgra(color.0)?;
        let mask_bits = bitmap_bgra(mask.0).map(|(_, _, m)| m);
        let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
        let mut out = Vec::with_capacity(bgra.len());
        for (i, p) in bgra.chunks_exact(4).enumerate() {
            let transparent_mask = mask_bits.as_ref().is_some_and(|m| m.get(i * 4).copied().unwrap_or(0) != 0);
            let alpha = if has_alpha {
                p[3]
            } else if transparent_mask && p[0] == 0 && p[1] == 0 && p[2] == 0 {
                0
            } else {
                255
            };
            out.extend_from_slice(&[p[2], p[1], p[0], alpha]);
        }
        (w, h, out)
    } else {
        // Monochrome: AND mask on top, XOR mask below.
        let (w, h2, m) = bitmap_bgra(mask.0)?;
        let h = h2 / 2;
        let bit = |plane: u32, x: u32, y: u32| m[(((plane * h + y) * w + x) * 4) as usize] != 0;
        let mut out = vec![0u8; (w * h * 4) as usize];
        let mut invert = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let px = &mut out[i * 4..i * 4 + 4];
                match (bit(0, x, y), bit(1, x, y)) {
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
        outline_inverted(&mut out, &invert, w as usize, h as usize);
        (w, h, out)
    };

    let mut id_input = Vec::with_capacity(rgba.len() + 16);
    for v in [w, h, hotspot.0, hotspot.1] {
        id_input.extend_from_slice(&v.to_le_bytes());
    }
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

/// Adds a white outline around inverting pixels so they stay visible on dark content.
pub(crate) fn outline_inverted(rgba: &mut [u8], invert: &[bool], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if rgba[i * 4 + 3] != 0 {
                continue;
            }
            let near = (y.saturating_sub(1)..=(y + 1).min(h - 1))
                .any(|ny| (x.saturating_sub(1)..=(x + 1).min(w - 1)).any(|nx| invert[ny * w + nx]));
            if near {
                rgba[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
}
