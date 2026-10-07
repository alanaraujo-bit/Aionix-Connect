//! Windows integration.

pub mod clipboard;
pub mod desktop;
pub mod dpapi;
pub mod sysinfo;

/// Makes the process per-monitor DPI aware (v2) so capture, display geometry and
/// input injection all use physical pixels. Must run before any window or display
/// API is used; safe to call repeatedly.
pub fn init_dpi_awareness() {
    use ::windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
