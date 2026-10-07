//! Captures frames from the primary display and prints timing statistics.
//! Run: cargo run -p aionix-capture --example capture_probe --release

#[cfg(windows)]
fn main() {
    use aionix_capture::windows::{FrameEvent, ScreenCapturer, enumerate_displays};
    tracing_subscriber::fmt().with_env_filter("debug").init();
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    for d in enumerate_displays().expect("enumerate") {
        println!(
            "display {}: {} {}x{} at ({},{}) primary={} rot={}",
            d.id, d.name, d.width, d.height, d.x, d.y, d.primary, d.rotation
        );
    }
    let (mut cap, display) = ScreenCapturer::open(1, None).expect("capturer");
    println!(
        "backend: {} gpu: {:?} display: {}",
        cap.backend().name(),
        cap.gpu(),
        display.name
    );
    let (mut frames, mut cursor_only, mut timeouts, mut total_us) = (0u32, 0u32, 0u32, 0u64);
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 3 {
        match cap.next_frame(100).expect("frame") {
            FrameEvent::Captured(f) => {
                if f.texture.is_some() {
                    frames += 1;
                    total_us += u64::from(f.capture_us);
                } else {
                    cursor_only += 1;
                }
                if let Some(s) = f.cursor_shape {
                    println!(
                        "cursor shape {}x{} hotspot ({},{})",
                        s.width, s.height, s.hotspot_x, s.hotspot_y
                    );
                }
            }
            FrameEvent::Timeout => timeouts += 1,
        }
    }
    println!(
        "frames={frames} cursor_only={cursor_only} timeouts={timeouts} avg_capture_us={}",
        if frames > 0 {
            total_us / u64::from(frames)
        } else {
            0
        }
    );
}

#[cfg(not(windows))]
fn main() {}
