//! Window icon rendered at startup (the brand mark), so the test build needs no image
//! assets.

/// Rounded accent square with a white "A" monogram, `size`×`size` RGBA.
pub fn brand_icon(size: u32, accent: [u8; 3]) -> egui::IconData {
    let s = size as f32;
    let radius = s * 0.24;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    // Monogram strokes in unit coordinates (0..1).
    let strokes = [
        ((0.27, 0.76), (0.50, 0.24)),
        ((0.50, 0.24), (0.73, 0.76)),
        ((0.37, 0.58), (0.63, 0.58)),
    ];
    let half_width = 0.055;
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Coverage of the rounded square (1 px anti-aliasing).
            let dx = (px - s / 2.0).abs() - (s / 2.0 - radius);
            let dy = (py - s / 2.0).abs() - (s / 2.0 - radius);
            let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - radius;
            let alpha = (0.5 - outside).clamp(0.0, 1.0);
            if alpha == 0.0 {
                continue;
            }
            let (ux, uy) = (px / s, py / s);
            let d = strokes
                .iter()
                .map(|&(a, b)| segment_distance((ux, uy), a, b))
                .fold(f32::MAX, f32::min);
            let white = ((half_width - d) * s + 0.5).clamp(0.0, 1.0);
            let i = ((y * size + x) * 4) as usize;
            for c in 0..3 {
                rgba[i + c] = (f32::from(accent[c]) * (1.0 - white) + 255.0 * white) as u8;
            }
            rgba[i + 3] = (alpha * 255.0) as u8;
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let (apx, apy) = (p.0 - a.0, p.1 - a.1);
    let t = ((apx * abx + apy * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + abx * t - p.0, a.1 + aby * t - p.1);
    (cx * cx + cy * cy).sqrt()
}
