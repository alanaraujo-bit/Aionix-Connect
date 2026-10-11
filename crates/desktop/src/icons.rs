//! Interface icons, drawn as vectors on a 16-unit grid: one stroke weight, round caps,
//! no fills except where a shape is meant to read as solid. No emoji or font glyphs.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint::PathShape, pos2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Home,
    Computers,
    Copy,
    Check,
    Search,
    User,
    SignOut,
    Pointer,
    Pulse,
    Fullscreen,
    ExitFullscreen,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Refresh,
    Download,
    Lock,
    Close,
    Phone,
    Shield,
    Key,
    More,
    Plus,
}

/// Draws `icon` centered in `rect` (square or not) in `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height());
    let s = side / 16.0;
    let origin = rect.center() - Vec2::splat(side / 2.0);
    let p = |x: f32, y: f32| origin + Vec2::new(x * s, y * s);
    let w = (1.5 * s).max(1.25);
    let stroke = Stroke::new(w, color);
    let line = |pts: Vec<Pos2>| polyline(painter, pts, stroke);
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        painter.rect_stroke(
            Rect::from_min_max(p(x0, y0), p(x1, y1)),
            r * s,
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    match icon {
        Icon::Home => {
            line(vec![p(2.5, 7.5), p(8.0, 3.0), p(13.5, 7.5)]);
            line(vec![p(4.0, 6.4), p(4.0, 13.0), p(12.0, 13.0), p(12.0, 6.4)]);
            line(vec![p(6.75, 13.0), p(6.75, 9.75), p(9.25, 9.75), p(9.25, 13.0)]);
        }
        Icon::Computers => {
            rrect(2.0, 3.0, 14.0, 10.5, 1.5);
            line(vec![p(8.0, 10.5), p(8.0, 13.0)]);
            line(vec![p(5.5, 13.25), p(10.5, 13.25)]);
        }
        Icon::Copy => {
            rrect(5.5, 5.5, 13.5, 13.5, 1.75);
            line(vec![p(10.5, 3.6), p(10.3, 3.0), p(9.6, 2.5), p(4.0, 2.5), p(3.1, 2.9), p(2.6, 3.6), p(2.5, 4.0), p(2.5, 9.6), p(2.9, 10.3), p(3.6, 10.5)]);
        }
        Icon::Check => {
            line(vec![p(3.0, 8.4), p(6.4, 11.6), p(13.0, 4.6)]);
        }
        Icon::Search => {
            painter.circle_stroke(p(7.0, 7.0), 4.5 * s, stroke);
            line(vec![p(10.4, 10.4), p(13.5, 13.5)]);
        }
        Icon::User => {
            painter.circle_stroke(p(8.0, 5.5), 2.75 * s, stroke);
            line(arc(p(8.0, 14.5), 5.25 * s, 200.0, 340.0));
        }
        Icon::SignOut => {
            line(vec![p(6.5, 2.75), p(3.75, 2.75), p(2.75, 3.75), p(2.75, 12.25), p(3.75, 13.25), p(6.5, 13.25)]);
            line(vec![p(6.5, 8.0), p(13.5, 8.0)]);
            line(vec![p(10.75, 5.25), p(13.5, 8.0), p(10.75, 10.75)]);
        }
        Icon::Pointer => {
            let pts = vec![p(4.0, 2.5), p(4.0, 12.75), p(6.6, 10.4), p(8.4, 14.0), p(10.0, 13.25), p(8.25, 9.75), p(11.75, 9.5)];
            painter.add(Shape::Path(PathShape::closed_line(pts, stroke)));
        }
        Icon::Pulse => {
            line(vec![p(1.75, 8.5), p(4.5, 8.5), p(6.25, 4.0), p(9.25, 12.5), p(11.0, 8.5), p(14.25, 8.5)]);
        }
        Icon::Fullscreen => {
            line(vec![p(2.75, 6.0), p(2.75, 2.75), p(6.0, 2.75)]);
            line(vec![p(10.0, 2.75), p(13.25, 2.75), p(13.25, 6.0)]);
            line(vec![p(13.25, 10.0), p(13.25, 13.25), p(10.0, 13.25)]);
            line(vec![p(6.0, 13.25), p(2.75, 13.25), p(2.75, 10.0)]);
        }
        Icon::ExitFullscreen => {
            line(vec![p(2.75, 6.0), p(6.0, 6.0), p(6.0, 2.75)]);
            line(vec![p(10.0, 2.75), p(10.0, 6.0), p(13.25, 6.0)]);
            line(vec![p(13.25, 10.0), p(10.0, 10.0), p(10.0, 13.25)]);
            line(vec![p(6.0, 13.25), p(6.0, 10.0), p(2.75, 10.0)]);
        }
        Icon::ChevronLeft => line(vec![p(10.0, 3.0), p(5.0, 8.0), p(10.0, 13.0)]),
        Icon::ChevronRight => line(vec![p(6.0, 3.0), p(11.0, 8.0), p(6.0, 13.0)]),
        Icon::ChevronDown => line(vec![p(3.0, 6.0), p(8.0, 11.0), p(13.0, 6.0)]),
        Icon::Refresh => {
            line(arc(p(8.0, 8.0), 5.25 * s, -60.0, 230.0));
            line(vec![p(13.5, 2.75), p(13.0, 5.6), p(10.2, 5.2)]);
        }
        Icon::Download => {
            line(vec![p(8.0, 2.5), p(8.0, 10.0)]);
            line(vec![p(4.75, 6.9), p(8.0, 10.1), p(11.25, 6.9)]);
            line(vec![p(2.75, 13.25), p(13.25, 13.25)]);
        }
        Icon::Lock => {
            rrect(3.25, 7.0, 12.75, 13.75, 1.5);
            line(vec![p(5.5, 7.0), p(5.5, 5.0)]);
            line(vec![p(10.5, 7.0), p(10.5, 5.0)]);
            line(arc(p(8.0, 5.0), 2.5 * s, 180.0, 360.0));
        }
        Icon::Close => {
            line(vec![p(4.0, 4.0), p(12.0, 12.0)]);
            line(vec![p(12.0, 4.0), p(4.0, 12.0)]);
        }
        Icon::Shield => {
            line(vec![p(8.0, 2.0), p(13.0, 4.0), p(13.0, 8.0), p(12.2, 10.7), p(10.4, 12.7), p(8.0, 14.0), p(5.6, 12.7), p(3.8, 10.7), p(3.0, 8.0), p(3.0, 4.0), p(8.0, 2.0)]);
            line(vec![p(5.75, 8.0), p(7.4, 9.6), p(10.4, 6.4)]);
        }
        Icon::Key => {
            painter.circle_stroke(p(5.25, 10.75), 2.75 * s, stroke);
            line(vec![p(7.2, 8.8), p(13.25, 2.75)]);
            line(vec![p(11.0, 5.0), p(12.75, 6.75)]);
            line(vec![p(9.25, 6.75), p(10.75, 8.25)]);
        }
        Icon::More => {
            for x in [3.5, 8.0, 12.5] {
                painter.circle_filled(p(x, 8.0), 1.25 * s, color);
            }
        }
        Icon::Plus => {
            line(vec![p(8.0, 3.0), p(8.0, 13.0)]);
            line(vec![p(3.0, 8.0), p(13.0, 8.0)]);
        }
        Icon::Phone => {
            // Handset (end call), drawn as a rounded bar with two ear pieces.
            line(vec![p(2.5, 9.75), p(3.0, 8.25), p(5.0, 7.0), p(8.0, 6.6), p(11.0, 7.0), p(13.0, 8.25), p(13.5, 9.75)]);
            line(vec![p(2.5, 9.75), p(5.0, 10.25), p(5.5, 8.6)]);
            line(vec![p(13.5, 9.75), p(11.0, 10.25), p(10.5, 8.6)]);
        }
    }
}

/// Open polyline with round caps (egui strokes have butt caps).
fn polyline(painter: &Painter, pts: Vec<Pos2>, stroke: Stroke) {
    if let (Some(a), Some(b)) = (pts.first().copied(), pts.last().copied()) {
        painter.add(Shape::Path(PathShape::line(pts, stroke)));
        let r = stroke.width / 2.0;
        painter.circle_filled(a, r, stroke.color);
        painter.circle_filled(b, r, stroke.color);
    }
}

/// Points along a circular arc, angles in degrees (0 = +x, clockwise on screen).
fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = ((to - from).abs() / 8.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            pos2(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}
