//! F12 debug overlay: a few lines of text and the frame-interval graph, drawn into the
//! sharp layer of the scene.

use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, Line, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::Color;

use crate::frame::DisplayList;
use crate::text::{TextEngine, TextStyle};

const TOLERANCE: f64 = 0.1;

/// Draws the overlay in the top-left corner. `intervals_ms` holds the most recent
/// frame intervals, oldest first; `period_ms` is the refresh period they are judged by.
pub fn draw(
    top: &mut DisplayList,
    text: &mut TextEngine,
    scale: f64,
    lines: &[String],
    intervals_ms: &[f32],
    period_ms: f32,
) {
    let px = |v: f64| v * scale;
    let font = px(12.0) as f32;
    let line_h = px(16.0);
    let pad = px(10.0);
    let width = px(460.0);
    let graph_h = px(90.0);
    let x0 = px(12.0);
    let y0 = px(12.0);
    let height = pad * 3.0 + line_h * lines.len() as f64 + graph_h;

    let panel = RoundedRect::new(x0, y0, x0 + width, y0 + height, px(8.0));
    top.fill(panel.to_path(TOLERANCE), Affine::IDENTITY, Color::from_rgba8(18, 18, 26, 215));

    let mut baseline = y0 + pad + line_h * 0.8;
    for line in lines {
        let shaped = text.line(line, font, TextStyle::Mono);
        let place = Affine::translate((x0 + pad, baseline));
        for run in &shaped.runs {
            top.glyphs(run, place, Color::from_rgb8(235, 235, 240));
        }
        baseline += line_h;
    }

    // Graph: one bar per frame, full height = 2.5 periods; lines at 1 and 2 periods.
    let gx0 = x0 + pad;
    let gx1 = x0 + width - pad;
    let gy1 = y0 + height - pad;
    let gy0 = gy1 - graph_h;
    let full = 2.5 * f64::from(period_ms.max(0.1));
    let y_of = |ms: f64| gy1 - (ms / full).min(1.0) * graph_h;

    let n = intervals_ms.len().max(1);
    let bar_w = (gx1 - gx0) / 240f64.max(n as f64);
    let mut ok = BezPath::new();
    let mut late = BezPath::new();
    for (i, &ms) in intervals_ms.iter().enumerate() {
        let x = gx1 - (n - i) as f64 * bar_w;
        let bar = Rect::new(x, y_of(f64::from(ms)), x + bar_w * 0.8, gy1);
        let target = if f64::from(ms) > 1.5 * f64::from(period_ms) { &mut late } else { &mut ok };
        target.extend(bar.path_elements(TOLERANCE));
    }
    top.fill(ok, Affine::IDENTITY, Color::from_rgb8(80, 200, 120));
    top.fill(late, Affine::IDENTITY, Color::from_rgb8(240, 80, 80));

    let rule = Arc::new(Stroke::new(px(1.0)));
    for k in [1.0, 2.0] {
        let y = y_of(k * f64::from(period_ms));
        let line = Line::new((gx0, y), (gx1, y));
        let mut path = BezPath::new();
        path.extend(line.path_elements(TOLERANCE));
        top.stroke(path, Affine::IDENTITY, rule.clone(), Color::from_rgba8(255, 255, 255, 110));
    }
    let frame = Rect::new(gx0, gy0, gx1, gy1);
    top.stroke(frame.to_path(TOLERANCE), Affine::IDENTITY, rule, Color::from_rgba8(255, 255, 255, 60));
}
