//! Text clipboard access with change detection.

use std::time::Duration;

use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
};

use crate::PlatformError;

/// Open/close guard. The clipboard is a shared resource; other applications may hold
/// it briefly, so opening retries for a short time.
struct Open;

impl Open {
    fn new() -> Result<Self, PlatformError> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(PlatformError::Os("clipboard busy".into()))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Monotonic counter that changes whenever any application modifies the clipboard.
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn get_text() -> Result<Option<String>, PlatformError> {
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)) }.is_err() {
        return Ok(None);
    }
    let _open = Open::new()?;
    let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }?;
    let hglobal = HGLOBAL(handle.0);
    let ptr = unsafe { GlobalLock(hglobal) } as *const u16;
    if ptr.is_null() {
        return Ok(None);
    }
    let max_units = unsafe { GlobalSize(hglobal) } / 2;
    let units = unsafe { std::slice::from_raw_parts(ptr, max_units) };
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..end]);
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Ok(Some(text))
}

/// Replaces the clipboard with `text`. Returns the new sequence number so callers can
/// ignore the change notification caused by their own write.
pub fn set_text(text: &str) -> Result<u32, PlatformError> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard()?;
        let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
        let dst = GlobalLock(mem) as *mut u16;
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err(PlatformError::Os("GlobalLock failed".into()));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(mem);
        if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(e.into());
        }
    }
    Ok(sequence_number())
}
