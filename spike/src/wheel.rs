//! The wheel picker of the spike: the static picture (slices, edges, labels), the
//! per-frame drawing, and the spin choreography.

use std::f64::consts::PI;
use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, Circle, CircleSegment, Line, Point, Shape, Stroke, Vec2};
use vello::peniko::Color;

use crate::frame::{DisplayList, Frame, Picture};
use crate::labels::{label_color, slice_color};
use crate::spin::{self, Slice, SpinPlan, Tick};
use crate::text::{TextEngine, TextStyle};

/// Screen angle of the pointer: 3 o'clock, so the drawn label reads left to right.
pub const POINTER_ANGLE: f64 = 0.0;
/// Labels below this size, in logical pixels, are not drawn (design doc: 9 px).
const MIN_LABEL_PX: f64 = 9.0;
/// Largest label size, in logical pixels.
const MAX_LABEL_PX: f64 = 30.0;
/// Path flattening tolerance, in pixels.
const TOLERANCE: f64 = 0.1;

const INK: Color = Color::from_rgb8(0x2b, 0x2d, 0x42);

/// Where the wheel sits in a frame, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub center: Point,
    pub radius: f64,
}

impl Geometry {
    pub fn for_frame(width: u32, height: u32) -> Self {
        let (w, h) = (f64::from(width), f64::from(height));
        Self { center: Point::new(w / 2.0, h / 2.0), radius: 0.43 * w.min(h) }
    }

    fn hub_radius(&self) -> f64 {
        0.075 * self.radius
    }
}

pub struct Wheel {
    pub entries: Vec<String>,
    pub weights: Vec<u32>,
    pub slices: Vec<Slice>,
    /// The cached picture and the (radius, scale) it was built for.
    picture: Option<((u64, u64), Arc<Picture>)>,
    /// How many labels the current picture draws (the rest are too narrow).
    pub labels_drawn: usize,
}

impl Wheel {
    pub fn new(entries: Vec<String>, weights: Vec<u32>) -> Self {
        assert_eq!(entries.len(), weights.len());
        let slices = spin::layout(&weights);
        Self { entries, weights, slices, picture: None, labels_drawn: 0 }
    }

    /// Index of the slice under the pointer at rotation `theta`.
    pub fn slice_at(&self, theta: f64) -> usize {
        spin::slice_at(&self.slices, POINTER_ANGLE, theta)
    }

    /// Draws the wheel at rotation `theta` (angular velocity `omega`, for the blur)
    /// with the pointer tilted by `tilt` radians.
    pub fn render(&mut self, frame: &mut Frame, text: &mut TextEngine, theta: f64, omega: f64, tilt: f64) {
        let g = Geometry::for_frame(frame.width, frame.height);
        let picture = self.picture(g.radius, frame.scale, text);
        let place = Affine::translate(g.center.to_vec2()) * Affine::rotate(theta);
        frame.moving.picture(&picture, place);
        if omega != 0.0 {
            frame.request_motion_blur(g.center, g.radius, omega);
        }
        draw_pointer(&mut frame.top, g, frame.scale, tilt);
        draw_hub(&mut frame.top, g, frame.scale);
    }

    /// The static wheel in wheel-local coordinates, centred on the origin. Rebuilt only
    /// when the radius or the scale changes.
    fn picture(&mut self, radius: f64, scale: f64, text: &mut TextEngine) -> Arc<Picture> {
        let key = (radius.to_bits(), scale.to_bits());
        if let Some((k, p)) = &self.picture {
            if *k == key {
                return p.clone();
            }
        }
        let (list, labels) = self.build(radius, scale, text);
        let picture = Picture::new(list);
        self.picture = Some((key, picture.clone()));
        self.labels_drawn = labels;
        picture
    }

    fn build(&self, radius: f64, scale: f64, text: &mut TextEngine) -> (DisplayList, usize) {
        let n = self.slices.len();
        let origin = Point::ZERO;
        let mut list = DisplayList::default();

        // A disk under the slices hides the hairline seams that antialiasing leaves
        // between adjacent fills.
        list.fill(Circle::new(origin, radius).to_path(TOLERANCE), Affine::IDENTITY, INK);

        for (i, s) in self.slices.iter().enumerate() {
            let segment = CircleSegment::new(origin, radius, 0.0, s.start, s.sweep);
            list.fill(segment.to_path(TOLERANCE), Affine::IDENTITY, slice_color(i, n));
        }

        // Edges only while slices are wide enough at the rim to show them.
        let narrowest = self.slices.iter().map(|s| s.sweep).fold(f64::INFINITY, f64::min);
        if n > 1 && narrowest * radius >= 6.0 * scale {
            let mut edges = BezPath::new();
            let inner = 0.075 * radius;
            for s in &self.slices {
                let dir = Vec2::from_angle(s.start);
                let line = Line::new(origin + dir * inner, origin + dir * radius);
                edges.extend(line.path_elements(TOLERANCE));
            }
            list.stroke(edges, Affine::IDENTITY, Stroke::new(1.5 * scale), Color::WHITE.with_alpha(0.55));
        }

        let mut labels = 0;
        let r_end = 0.93 * radius;
        let r_min = 0.075 * radius + 6.0 * scale;
        let max_size = (MAX_LABEL_PX * scale).min(0.075 * radius);
        let min_size = MIN_LABEL_PX * scale;
        for (i, s) in self.slices.iter().enumerate() {
            let Some(line) = fit_label(text, &self.entries[i], s.sweep, r_end, r_min, max_size, min_size)
            else {
                continue;
            };
            labels += 1;
            let x0 = r_end - f64::from(line.width);
            let y0 = f64::from(line.ascent - line.descent) / 2.0;
            let place = Affine::rotate(s.mid()) * Affine::translate((x0, y0));
            let color = label_color(slice_color(i, n));
            for run in &line.runs {
                list.glyphs(run, place, color);
            }
        }

        let rim = (0.012 * radius).max(2.0 * scale);
        list.stroke(Circle::new(origin, radius).to_path(TOLERANCE), Affine::IDENTITY, Stroke::new(rim), INK);
        (list, labels)
    }
}

/// Lays out a label that runs from near the rim towards the centre. Its size is the
/// smaller of the maximum and what fits the slice's width at the label's inner end;
/// it is cut with an ellipsis when longer than the room between hub and rim.
fn fit_label(
    text: &mut TextEngine,
    entry: &str,
    sweep: f64,
    r_end: f64,
    r_min: f64,
    max_size: f64,
    min_size: f64,
) -> Option<crate::text::TextLine> {
    let chord_at = |r: f64| 2.0 * r * (sweep.min(PI) / 2.0).sin();
    // Cheap bound before any shaping: even at the rim the slice may be too thin.
    if 0.9 * chord_at(r_end) / 1.4 < min_size {
        return None;
    }
    let room = r_end - r_min;
    // Shape once: unhinted, unquantized lines scale linearly with the size (measured to
    // 3e-5 px), so width and height at any size follow from these per-pixel values.
    let base = text.line(entry, max_size as f32, TextStyle::Label);
    let width_per_px = f64::from(base.width) / max_size;
    let height_per_px = (f64::from(base.ascent + base.descent) / max_size).max(1e-3);
    let mut size = max_size;
    for _ in 0..8 {
        let len = (width_per_px * size).min(room);
        let fit = size.min(0.9 * chord_at(r_end - len) / height_per_px);
        if fit >= 0.98 * size {
            break;
        }
        size = fit;
    }
    if size < min_size {
        return None;
    }
    text.line_fitting(entry, size as f32, TextStyle::Label, room as f32)
}

fn draw_pointer(top: &mut DisplayList, g: Geometry, scale: f64, tilt: f64) {
    let r = g.radius;
    let mut path = BezPath::new();
    path.move_to((0.93 * r, 0.0));
    path.line_to((1.09 * r, -0.055 * r));
    path.line_to((1.09 * r, 0.055 * r));
    path.close_path();
    let pivot = Point::new(1.07 * r, 0.0);
    let place = Affine::translate(g.center.to_vec2())
        * Affine::rotate(POINTER_ANGLE)
        * Affine::rotate_about(tilt, pivot);
    let path = Arc::new(path);
    top.fill(path.clone(), place, INK);
    top.stroke(path, place, Stroke::new(2.0 * scale), Color::WHITE);
}

fn draw_hub(top: &mut DisplayList, g: Geometry, scale: f64) {
    let hub = Circle::new(g.center, g.hub_radius()).to_path(TOLERANCE);
    let hub = Arc::new(hub);
    top.fill(hub.clone(), Affine::IDENTITY, Color::WHITE);
    top.stroke(hub, Affine::IDENTITY, Stroke::new(3.0 * scale), INK);
}

/// A spin from rest to rest: every visual quantity is a function of t alone.
pub struct Spin {
    pub plan: SpinPlan,
    pub ticks: Vec<Tick>,
    /// How hard each tick's edge kicks the pointer, in radians.
    kicks: Vec<f64>,
    pub winner: usize,
}

impl Spin {
    /// `u` places the landing point across the winning slice; it comes from the
    /// cosmetic generator and never affects which slice wins.
    pub fn new(wheel: &Wheel, winner: usize, theta0: f64, duration: f64, omega: f64, u: f64) -> Self {
        let phi = spin::landing_angle(wheel.slices[winner], u);
        let plan = SpinPlan::new(theta0, phi, POINTER_ANGLE, duration, omega);
        let ticks = plan.ticks(&wheel.slices, POINTER_ANGLE);
        // An edge between narrow slices is a small peg: it barely moves the pointer.
        // Full strength from slices of 1/12 of the wheel up.
        let n = wheel.slices.len();
        let kicks = ticks
            .iter()
            .map(|k| {
                let narrower = wheel.slices[k.edge].sweep.min(wheel.slices[(k.edge + n - 1) % n].sweep);
                KICK * (narrower / (std::f64::consts::TAU / 12.0)).min(1.0)
            })
            .collect();
        Self { plan, ticks, kicks, winner }
    }

    pub fn duration(&self) -> f64 {
        self.plan.duration
    }

    pub fn theta(&self, t: f64) -> f64 {
        self.plan.theta(t)
    }

    pub fn omega(&self, t: f64) -> f64 {
        if t >= self.plan.duration { 0.0 } else { self.plan.omega(t) }
    }

    pub fn rest(&self) -> f64 {
        self.plan.rest()
    }

    /// Pointer deflection: each tick kicks an underdamped spring, and the response is
    /// summed in closed form over the ticks of the last few hundred milliseconds.
    /// Negative: the edges move down past the 3 o'clock pointer and push its tip down.
    pub fn pointer_tilt(&self, t: f64) -> f64 {
        const WINDOW: f64 = 0.35;
        const FREQ: f64 = 11.0; // Hz
        const ZETA: f64 = 0.32;
        let w = 2.0 * PI * FREQ;
        let wd = w * (1.0 - ZETA * ZETA).sqrt();
        let lo = self.ticks.partition_point(|k| k.t < t - WINDOW);
        let hi = self.ticks.partition_point(|k| k.t <= t);
        let sum: f64 = self.ticks[lo..hi]
            .iter()
            .zip(&self.kicks[lo..hi])
            .map(|(k, kick)| {
                let s = t - k.t;
                kick * (-ZETA * w * s).exp() * (wd * s).sin()
            })
            .sum();
        (-sum).clamp(-0.35, 0.35)
    }
}

/// Pointer kick of an edge between two slices of at least 1/12 of the wheel, in radians.
const KICK: f64 = 0.22;
