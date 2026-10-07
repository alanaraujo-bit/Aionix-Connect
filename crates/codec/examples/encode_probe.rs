//! Captures the primary display, encodes H.264 for a few seconds and reports timings.
//! Writes the Annex B stream to `target/encode_probe.h264`.
//!
//! cargo run -p aionix-codec --example encode_probe --release [-- --software] [-- --scale 0.5]

#[cfg(windows)]
fn main() {
    use aionix_capture::windows::{FrameEvent, ScreenCapturer};
    use aionix_codec::{
        EncoderConfig, EncoderPreference, even,
        windows::{H264Decoder, H264Encoder, VideoConverter, list_encoders, psnr_luma},
    };
    use std::{io::Write, time::Instant};

    tracing_subscriber::fmt()
        .with_env_filter("info,aionix_codec=debug")
        .init();
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let args: Vec<String> = std::env::args().collect();
    let software = args.iter().any(|a| a == "--software");
    let scale: f64 = args
        .iter()
        .position(|a| a == "--scale")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);

    let (mut cap, display) = ScreenCapturer::open(1, None).expect("capturer");
    let gpu = cap.gpu().clone();
    println!(
        "capture: {} on {} ({}x{})",
        cap.backend().name(),
        gpu.adapter_name,
        display.width,
        display.height
    );
    for e in list_encoders(Some(gpu.adapter_luid)) {
        println!("  encoder available: {} (hardware={})", e.name, e.hardware);
    }
    let (sw, sh) = cap.size();
    let out = (
        even((f64::from(sw) * scale) as u32),
        even((f64::from(sh) * scale) as u32),
    );
    let mut conv =
        VideoConverter::new(&gpu.device, &gpu.context, (sw, sh), out).expect("converter");
    let cfg = EncoderConfig {
        width: out.0,
        height: out.1,
        fps: 60,
        bitrate_kbps: 8000,
    };
    let name_contains = args
        .iter()
        .position(|a| a == "--encoder")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let pref = EncoderPreference {
        allow_hardware: !software,
        name_contains,
    };
    let mut enc =
        H264Encoder::new(&gpu.device, &gpu.context, gpu.adapter_luid, cfg, &pref).expect("encoder");
    let mut dec = H264Decoder::new().expect("decoder");
    let mut sources = std::collections::VecDeque::new();
    let (mut psnr_sum, mut psnr_n, mut psnr_min) = (0.0f64, 0u32, f64::MAX);
    println!("using encoder: {:?}", enc.info());

    let mut file = std::fs::File::create("target/encode_probe.h264").unwrap();
    let start = Instant::now();
    let (mut frames, mut bytes, mut convert_us, mut encode_us, mut keyframes) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut first = true;
    let (mut same_call, mut late) = (0u32, 0u32);
    let mut texture = None;
    while start.elapsed().as_secs() < 4 {
        // Re-encode the last image at least every 100 ms so a static screen still
        // exercises the encoder.
        match cap.next_frame(100).expect("capture") {
            FrameEvent::Captured(f) => {
                if let Some(t) = f.texture {
                    texture = Some(t)
                }
            }
            FrameEvent::Timeout => texture = Some(cap.last_texture()),
        }
        let Some(tex) = texture.take() else { continue };
        let t0 = Instant::now();
        let nv12 = conv.convert(&tex).expect("convert");
        convert_us += t0.elapsed().as_micros() as u64;
        let pts = start.elapsed().as_micros() as u64;
        sources.push_back(conv.read_nv12(&nv12).expect("readback"));
        let aus = enc.encode(&nv12, pts, first).expect("encode");
        if aus.iter().any(|a| a.pts_us == pts) {
            same_call += 1
        } else {
            late += 1
        }
        for au in aus {
            frames += 1;
            bytes += au.data.len() as u64;
            encode_us += u64::from(au.encode_us);
            keyframes += u64::from(au.keyframe);
            file.write_all(&au.data).unwrap();
            for pic in dec.decode(&au.data).expect("decode") {
                if let Some(src) = sources.pop_front() {
                    if psnr_n == 0 {
                        let mean = |v: &[u8]| {
                            v.iter()
                                .take((pic.width * pic.height) as usize)
                                .map(|x| u64::from(*x))
                                .sum::<u64>()
                                / u64::from(pic.width * pic.height)
                        };
                        println!(
                            "first decoded {}x{} len {} src len {} mean src {} dec {}",
                            pic.width,
                            pic.height,
                            pic.nv12.len(),
                            src.len(),
                            mean(&src),
                            mean(&pic.nv12)
                        );
                    }
                    let p = psnr_luma(&src, &pic.nv12, pic.width, pic.height);
                    psnr_sum += p.min(99.0);
                    psnr_n += 1;
                    psnr_min = psnr_min.min(p);
                }
            }
        }
        first = false;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{}x{}: {frames} frames in {secs:.1}s ({:.1} fps), {keyframes} keyframes, {:.0} kbps, avg convert {} us, avg encode {} us",
        out.0,
        out.1,
        frames as f64 / secs,
        bytes as f64 * 8.0 / secs / 1000.0,
        convert_us / frames.max(1),
        encode_us / frames.max(1)
    );
    println!("outputs in same call: {same_call}, delayed: {late}");
    println!(
        "decoded {psnr_n} frames, luma PSNR avg {:.1} dB, min {:.1} dB",
        psnr_sum / f64::from(psnr_n.max(1)),
        psnr_min
    );
}

#[cfg(not(windows))]
fn main() {}
