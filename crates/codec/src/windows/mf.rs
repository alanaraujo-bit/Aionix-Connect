//! Media Foundation helpers shared by the encoder and decoder.

use std::sync::OnceLock;

use windows::{
    Win32::{
        Foundation::VARIANT_BOOL,
        Media::MediaFoundation::{
            IMFActivate, IMFAttributes, IMFMediaBuffer, IMFSample, MFSTARTUP_FULL, MFStartup,
            MF_VERSION, MFT_FRIENDLY_NAME_Attribute,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
    core::PWSTR,
};

use crate::CodecError;

/// Starts Media Foundation once per process (and COM on the calling thread).
pub fn startup() -> Result<(), CodecError> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    unsafe {
        // Each thread using MF objects needs COM; an existing apartment is fine.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string()) })
        .clone()
        .map_err(CodecError::Platform)
}

pub(crate) fn pack_u64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

pub(crate) fn variant_u32(v: u32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    var
}

pub(crate) fn variant_bool(v: bool) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if v { -1 } else { 0 });
    }
    var
}

pub(crate) fn friendly_name(activate: &IMFActivate) -> String {
    allocated_string(&activate.clone().into(), &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "H.264 encoder".into())
}

pub(crate) fn allocated_string(attrs: &IMFAttributes, key: &windows::core::GUID) -> Option<String> {
    let mut ptr = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        attrs.GetAllocatedString(key, &mut ptr, &mut len).ok()?;
        let s = ptr.to_string().ok();
        CoTaskMemFree(Some(ptr.0 as *const _));
        s
    }
}

/// Copies the contents of an MF sample into a `Vec`.
pub(crate) fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, CodecError> {
    let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len))? };
    let data = unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec();
    unsafe { buffer.Unlock()? };
    Ok(data)
}

/// Takes ownership of an MFTEnum activation array.
pub(crate) fn take_activates(ptr: *mut Option<IMFActivate>, count: u32) -> Vec<IMFActivate> {
    if ptr.is_null() {
        return Vec::new();
    }
    let list = unsafe { std::slice::from_raw_parts_mut(ptr, count as usize) }
        .iter_mut()
        .filter_map(Option::take)
        .collect();
    unsafe { CoTaskMemFree(Some(ptr as *const _)) };
    list
}

/// Wrapper to move free-threaded Media Foundation objects across threads.
pub(crate) struct SendCom<T>(pub T);
// SAFETY: only used for Media Foundation objects documented as free-threaded
// (MFT event generators), accessed from one other thread.
unsafe impl<T> Send for SendCom<T> {}
