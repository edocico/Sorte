//! The drawing contract between pickers and the stage (`Frame` in the design doc, § Modello a
//! plugin dei picker, "Forma di Frame").
//!
//! Pickers never talk to a renderer: they record drawing commands into display lists, and
//! `sorte-stage` replays them with Vello on the GPU or with vello_cpu. The types are kurbo's and
//! peniko's, the same versions Vello uses, so the stage passes them through unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kurbo::{Affine, BezPath, PathEl, Point, Size, Stroke};
use peniko::{Color, FontData};

/// The size of the scene a picker draws into: the size of the frames to come.
///
/// The controller never asks for a scene without area (a minimized window draws nothing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// In physical pixels.
    pub width: u32,
    /// In physical pixels.
    pub height: u32,
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl Viewport {
    /// The size in physical pixels.
    pub fn size(&self) -> Size {
        Size::new(self.width.into(), self.height.into())
    }
}

/// One glyph, positioned in its run's coordinate space (y down, baseline at y = 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// Glyph id in the run's font.
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// Glyphs that share a font, a size and a variation: what both renderers draw in one call.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    pub font: FontData,
    /// Font size in pixels of the run's coordinate space.
    pub font_size: f32,
    /// Variable-font coordinates (F2Dot14), empty for static fonts.
    pub normalized_coords: Arc<[i16]>,
    pub glyphs: Arc<[Glyph]>,
}

/// One drawing command. Paths and pictures sit behind `Arc`, so a picker builds them once and
/// pushes them again every frame for the cost of a reference count.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Fill { path: Arc<BezPath>, transform: Affine, color: Color },
    Stroke { path: Arc<BezPath>, transform: Affine, style: Arc<Stroke>, color: Color },
    Glyphs { run: GlyphRun, transform: Affine, color: Color },
    Picture { picture: Arc<Picture>, transform: Affine },
}

/// An ordered list of drawing commands, painted first to last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    cmds: Vec<Cmd>,
}

impl DisplayList {
    pub fn fill(&mut self, path: impl Into<Arc<BezPath>>, transform: Affine, color: Color) {
        self.cmds.push(Cmd::Fill { path: path.into(), transform, color });
    }

    pub fn stroke(
        &mut self,
        path: impl Into<Arc<BezPath>>,
        transform: Affine,
        style: impl Into<Arc<Stroke>>,
        color: Color,
    ) {
        self.cmds.push(Cmd::Stroke { path: path.into(), transform, style: style.into(), color });
    }

    pub fn glyphs(&mut self, run: &GlyphRun, transform: Affine, color: Color) {
        self.cmds.push(Cmd::Glyphs { run: run.clone(), transform, color });
    }

    pub fn picture(&mut self, picture: &Arc<Picture>, transform: Affine) {
        self.cmds.push(Cmd::Picture { picture: picture.clone(), transform });
    }

    pub fn cmds(&self) -> &[Cmd] {
        &self.cmds
    }
}

/// A display list recorded once and drawn many times under a transform, such as the wheel with
/// its labels.
///
/// The stage encodes a picture once and reuses the encoding every frame, which Vello needs:
/// it re-expands colour emoji on the CPU at every draw. Each picture gets a new id, so the stage
/// can cache whatever it derives from it under that id. Pickers therefore record pictures in
/// `prepare` or `choreograph` and draw them again, never record them while rendering: the
/// contract suite fails a render that records a new picture for a frame it already drew.
#[derive(Debug)]
pub struct Picture {
    id: u64,
    list: DisplayList,
}

impl Picture {
    pub fn new(list: DisplayList) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self { id: NEXT.fetch_add(1, Ordering::Relaxed), list })
    }

    /// Unique among the pictures of this process.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn cmds(&self) -> &[Cmd] {
        self.list.cmds()
    }
}

/// Two pictures are equal when they draw the same commands, whatever their ids: a frame is the
/// same frame whether its picker reused a picture or recorded an identical one.
impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id || self.list == other.list
    }
}

/// Rotational motion blur for the moving layer, requested by the choreography.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionBlur {
    /// Centre of rotation, in physical pixels.
    pub center: Point,
    /// How far the moving content reaches from the centre, in physical pixels.
    pub radius: f64,
    /// Angular velocity in rad/s, clockwise positive.
    pub omega: f64,
}

/// One frame of the scene, in physical pixels.
///
/// `moving` holds what may be motion-blurred (the wheel), `top` what stays sharp above it
/// (pointer, hub, overlays). A backend that cannot blur draws `moving`, then `top`, into the
/// same target.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel.
    pub scale: f64,
    pub background: Color,
    /// Refresh period in seconds: the blur covers the arc travelled in half of it.
    pub period: f64,
    pub moving: DisplayList,
    pub top: DisplayList,
    pub motion_blur: Option<MotionBlur>,
}

impl Frame {
    pub fn new(width: u32, height: u32, scale: f64, background: Color, period: f64) -> Self {
        Self {
            width,
            height,
            scale,
            background,
            period,
            moving: DisplayList::default(),
            top: DisplayList::default(),
            motion_blur: None,
        }
    }

    /// Asks the stage to blur the moving layer as it rotates about `center` at `omega` rad/s.
    pub fn request_motion_blur(&mut self, center: Point, radius: f64, omega: f64) {
        self.motion_blur = Some(MotionBlur { center, radius, omega });
    }

    /// Arc swept during half a refresh period, in radians: the blur's shutter angle.
    pub fn blur_angle(&self) -> f64 {
        self.motion_blur.map_or(0.0, |b| b.omega * self.period * 0.5)
    }

    /// The size this frame is drawn at.
    pub fn viewport(&self) -> Viewport {
        Viewport { width: self.width, height: self.height, scale: self.scale }
    }

    /// Whether each picture this frame draws is the very picture (by id) `other` draws in the
    /// same place. Two frames that compare equal may still differ here, when one was drawn
    /// with a picture recorded again: the stage would encode it again.
    pub(crate) fn same_pictures(&self, other: &Frame) -> bool {
        let same = |a: &[Cmd], b: &[Cmd]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|pair| match pair {
                    (Cmd::Picture { picture: p, .. }, Cmd::Picture { picture: q, .. }) => p.id == q.id,
                    _ => true,
                })
        };
        same(self.moving.cmds(), other.moving.cmds()) && same(self.top.cmds(), other.top.cmds())
    }

    /// The first part of this frame that differs from `other` by more than rounding, named, or
    /// `None` when they draw the same: the same commands, with coordinates, transforms,
    /// colours and blur within a relative `tolerance` of each other.
    pub(crate) fn difference(&self, other: &Frame, tolerance: f64) -> Option<String> {
        let near = |a: f64, b: f64| close(a, b, tolerance);
        if (self.width, self.height) != (other.width, other.height)
            || !near(self.scale, other.scale)
            || !near(self.period, other.period)
        {
            return Some("the frame's size".to_owned());
        }
        if !close_color(self.background, other.background) {
            return Some("the background".to_owned());
        }
        let blur_matches = match (self.motion_blur, other.motion_blur) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                close_point(a.center, b.center, tolerance) && near(a.radius, b.radius) && near(a.omega, b.omega)
            }
            _ => false,
        };
        if !blur_matches {
            return Some(format!("the motion blur ({:?}, against {:?})", self.motion_blur, other.motion_blur));
        }
        let layer = |name: &str, a: &[Cmd], b: &[Cmd]| {
            if a.len() != b.len() {
                return Some(format!("the {name} layer's command count ({} against {})", a.len(), b.len()));
            }
            let index = a.iter().zip(b).position(|(a, b)| !close_cmd(a, b, tolerance))?;
            Some(format!("command {index} of the {name} layer"))
        };
        layer("moving", self.moving.cmds(), other.moving.cmds())
            .or_else(|| layer("top", self.top.cmds(), other.top.cmds()))
    }

    /// The first non-finite number a picker put in this frame, named, or `None` when every
    /// coordinate, colour and size is finite. A NaN reaches the renderer as garbage at best.
    pub(crate) fn non_finite(&self) -> Option<&'static str> {
        if let Some(blur) = self.motion_blur
            && !(finite_point(blur.center) && blur.radius.is_finite() && blur.omega.is_finite())
        {
            return Some("motion blur");
        }
        non_finite_cmds(self.moving.cmds()).or_else(|| non_finite_cmds(self.top.cmds()))
    }
}

fn non_finite_cmds(cmds: &[Cmd]) -> Option<&'static str> {
    cmds.iter().find_map(|cmd| match cmd {
        Cmd::Fill { path, transform, color } => {
            let ok = finite_path(path) && finite_affine(*transform) && finite_color(*color);
            (!ok).then_some("fill")
        }
        Cmd::Stroke { path, transform, style, color } => {
            let ok = finite_path(path)
                && finite_affine(*transform)
                && style.width.is_finite()
                && style.miter_limit.is_finite()
                && style.dash_offset.is_finite()
                && style.dash_pattern.iter().all(|d| d.is_finite())
                && finite_color(*color);
            (!ok).then_some("stroke")
        }
        Cmd::Glyphs { run, transform, color } => {
            let ok = run.font_size.is_finite()
                && run.glyphs.iter().all(|g| g.x.is_finite() && g.y.is_finite())
                && finite_affine(*transform)
                && finite_color(*color);
            (!ok).then_some("glyphs")
        }
        Cmd::Picture { picture, transform } => {
            if finite_affine(*transform) {
                non_finite_cmds(picture.cmds())
            } else {
                Some("picture transform")
            }
        }
    })
}

fn finite_point(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn finite_affine(a: Affine) -> bool {
    a.as_coeffs().iter().all(|c| c.is_finite())
}

fn finite_color(c: Color) -> bool {
    c.components.iter().all(|c| c.is_finite())
}

fn finite_path(path: &BezPath) -> bool {
    path.elements().iter().all(|el| match *el {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => finite_point(p),
        PathEl::QuadTo(a, b) => finite_point(a) && finite_point(b),
        PathEl::CurveTo(a, b, c) => finite_point(a) && finite_point(b) && finite_point(c),
        PathEl::ClosePath => true,
    })
}

/// Colour components are `f32` in [0, 1]: closer than this is rounding, a visible step is
/// 1/255 at least.
const COLOR_TOLERANCE: f32 = 1e-5;

/// `a` and `b` within `tolerance` of the larger magnitude, or of 1 near zero.
fn close(a: f64, b: f64, tolerance: f64) -> bool {
    a == b || (a - b).abs() <= tolerance * a.abs().max(b.abs()).max(1.0)
}

fn close_point(a: Point, b: Point, tolerance: f64) -> bool {
    close(a.x, b.x, tolerance) && close(a.y, b.y, tolerance)
}

fn close_affine(a: Affine, b: Affine, tolerance: f64) -> bool {
    a.as_coeffs().iter().zip(b.as_coeffs()).all(|(&a, b)| close(a, b, tolerance))
}

fn close_color(a: Color, b: Color) -> bool {
    a.components.iter().zip(b.components).all(|(&a, b)| a == b || (a - b).abs() <= COLOR_TOLERANCE)
}

fn close_path(a: &BezPath, b: &BezPath, tolerance: f64) -> bool {
    let near = |p, q| close_point(p, q, tolerance);
    a.elements().len() == b.elements().len()
        && a.elements().iter().zip(b.elements()).all(|pair| match (*pair.0, *pair.1) {
            (PathEl::MoveTo(p), PathEl::MoveTo(q)) | (PathEl::LineTo(p), PathEl::LineTo(q)) => near(p, q),
            (PathEl::QuadTo(p1, p2), PathEl::QuadTo(q1, q2)) => near(p1, q1) && near(p2, q2),
            (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => near(p1, q1) && near(p2, q2) && near(p3, q3),
            (PathEl::ClosePath, PathEl::ClosePath) => true,
            _ => false,
        })
}

fn close_stroke(a: &Stroke, b: &Stroke, tolerance: f64) -> bool {
    let shape = |s: &Stroke| (s.join, s.start_cap, s.end_cap, s.dash_pattern.len());
    shape(a) == shape(b)
        && close(a.width, b.width, tolerance)
        && close(a.miter_limit, b.miter_limit, tolerance)
        && close(a.dash_offset, b.dash_offset, tolerance)
        && a.dash_pattern.iter().zip(&b.dash_pattern).all(|(&a, &b)| close(a, b, tolerance))
}

fn close_run(a: &GlyphRun, b: &GlyphRun, tolerance: f64) -> bool {
    let near = |a: f32, b: f32| close(a.into(), b.into(), tolerance);
    a.font == b.font
        && a.normalized_coords == b.normalized_coords
        && near(a.font_size, b.font_size)
        && a.glyphs.len() == b.glyphs.len()
        && a.glyphs.iter().zip(b.glyphs.iter()).all(|(g, h)| g.id == h.id && near(g.x, h.x) && near(g.y, h.y))
}

fn close_cmd(a: &Cmd, b: &Cmd, tolerance: f64) -> bool {
    match (a, b) {
        (Cmd::Fill { path: p, transform: s, color: c }, Cmd::Fill { path: q, transform: t, color: d }) => {
            close_path(p, q, tolerance) && close_affine(*s, *t, tolerance) && close_color(*c, *d)
        }
        (
            Cmd::Stroke { path: p, transform: s, style: x, color: c },
            Cmd::Stroke { path: q, transform: t, style: y, color: d },
        ) => {
            close_path(p, q, tolerance)
                && close_affine(*s, *t, tolerance)
                && close_stroke(x, y, tolerance)
                && close_color(*c, *d)
        }
        (Cmd::Glyphs { run: r, transform: s, color: c }, Cmd::Glyphs { run: q, transform: t, color: d }) => {
            close_run(r, q, tolerance) && close_affine(*s, *t, tolerance) && close_color(*c, *d)
        }
        (Cmd::Picture { picture: p, transform: s }, Cmd::Picture { picture: q, transform: t }) => {
            p == q && close_affine(*s, *t, tolerance)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use kurbo::{Circle, Shape};

    use super::*;

    fn disk() -> BezPath {
        Circle::new((10.0, 10.0), 5.0).to_path(0.1)
    }

    fn frame_with(moving: DisplayList) -> Frame {
        let mut frame = Frame::new(100, 80, 2.0, Color::BLACK, 1.0 / 60.0);
        frame.moving = moving;
        frame
    }

    #[test]
    fn pictures_with_the_same_commands_are_equal_whatever_their_ids() {
        let mut list = DisplayList::default();
        list.fill(disk(), Affine::IDENTITY, Color::WHITE);
        let (a, b) = (Picture::new(list.clone()), Picture::new(list.clone()));
        assert_ne!(a.id(), b.id());
        assert_eq!(a, b);
        list.fill(disk(), Affine::translate((1.0, 0.0)), Color::WHITE);
        assert_ne!(*a, *Picture::new(list));
    }

    #[test]
    fn frames_compare_by_content() {
        let mut list = DisplayList::default();
        list.fill(disk(), Affine::rotate(0.5), Color::WHITE);
        let mut other = list.clone();
        assert_eq!(frame_with(list.clone()), frame_with(other.clone()));
        other.fill(disk(), Affine::rotate(0.5 + 1e-12), Color::WHITE);
        assert_ne!(frame_with(list), frame_with(other));
    }

    #[test]
    fn a_picture_recorded_again_is_equal_but_not_the_same_picture() {
        let mut list = DisplayList::default();
        list.fill(disk(), Affine::IDENTITY, Color::WHITE);
        let picture = Picture::new(list.clone());
        let drawn = |picture: &Arc<Picture>| {
            let mut moving = DisplayList::default();
            moving.fill(disk(), Affine::IDENTITY, Color::BLACK);
            moving.picture(picture, Affine::rotate(0.5));
            frame_with(moving)
        };
        let frame = drawn(&picture);
        assert!(frame.same_pictures(&drawn(&picture)));
        let recorded_again = drawn(&Picture::new(list));
        assert_eq!(frame, recorded_again);
        assert!(!frame.same_pictures(&recorded_again));

        let mut top = drawn(&picture);
        top.top.picture(&Picture::new(DisplayList::default()), Affine::IDENTITY);
        let mut top_again = drawn(&picture);
        top_again.top.picture(&Picture::new(DisplayList::default()), Affine::IDENTITY);
        assert!(!top.same_pictures(&top_again), "pictures on the top layer count too");
    }

    #[test]
    fn frames_within_rounding_of_each_other_draw_the_same() {
        let picture = Picture::new(DisplayList::default());
        let frame = |angle: f64, alpha: f32, omega: f64| {
            let mut moving = DisplayList::default();
            moving.picture(&picture, Affine::translate((640.0, 400.0)) * Affine::rotate(angle));
            moving.stroke(disk(), Affine::rotate(angle), Stroke::new(1.5), Color::WHITE.with_alpha(alpha));
            let mut frame = frame_with(moving);
            frame.top.fill(disk(), Affine::IDENTITY, Color::WHITE);
            frame.request_motion_blur(Point::new(640.0, 400.0), 300.0, omega);
            frame
        };
        let reference = frame(1234.5, 0.5, 40.0);
        assert_eq!(reference.difference(&reference.clone(), 1e-6), None);
        // A few ulps apart, as when a replan recomputes the state it starts from.
        let rounded = frame(1234.5 + 1e-12, 0.5 + 1e-7, 40.0 * (1.0 + 1e-14));
        assert_ne!(reference, rounded);
        assert_eq!(reference.difference(&rounded, 1e-6), None);

        let differs = |other: &Frame| reference.difference(other, 1e-6).unwrap_or_default();
        assert_eq!(differs(&frame(1234.5 + 0.01, 0.5, 40.0)), "command 0 of the moving layer");
        assert_eq!(differs(&frame(1234.5, 0.6, 40.0)), "command 1 of the moving layer");
        assert!(differs(&frame(1234.5, 0.5, 30.0)).starts_with("the motion blur"));
        let mut still = reference.clone();
        still.motion_blur = None;
        assert!(differs(&still).starts_with("the motion blur"));
        let mut other_top = reference.clone();
        other_top.top.fill(disk(), Affine::translate((0.0, 1.0)), Color::WHITE);
        assert_eq!(differs(&other_top), "the top layer's command count (1 against 2)");
        let mut moved_top = frame(1234.5, 0.5, 40.0);
        moved_top.top = DisplayList::default();
        moved_top.top.fill(disk(), Affine::translate((0.0, 1.0)), Color::WHITE);
        assert_eq!(differs(&moved_top), "command 0 of the top layer");
        let mut resized = reference.clone();
        resized.width += 1;
        assert_eq!(differs(&resized), "the frame's size");
    }

    #[test]
    fn frames_with_other_commands_paths_or_glyphs_differ() {
        let close = |a: &DisplayList, b: &DisplayList| frame_with(a.clone()).difference(&frame_with(b.clone()), 1e-6);
        let mut fill = DisplayList::default();
        fill.fill(disk(), Affine::IDENTITY, Color::WHITE);
        let mut stroke = DisplayList::default();
        stroke.stroke(disk(), Affine::IDENTITY, Stroke::new(1.0), Color::WHITE);
        assert!(close(&fill, &stroke).is_some(), "a fill is not a stroke");
        let mut wider = DisplayList::default();
        wider.stroke(disk(), Affine::IDENTITY, Stroke::new(2.0), Color::WHITE);
        assert!(close(&stroke, &wider).is_some());
        let mut dashed = DisplayList::default();
        dashed.stroke(disk(), Affine::IDENTITY, Stroke::new(1.0).with_dashes(0.0, [2.0, 1.0]), Color::WHITE);
        assert!(close(&stroke, &dashed).is_some());
        let mut bigger = DisplayList::default();
        bigger.fill(Circle::new((10.0, 10.0), 5.5).to_path(0.1), Affine::IDENTITY, Color::WHITE);
        assert!(close(&fill, &bigger).is_some());

        let font = FontData::new(peniko::Blob::from(vec![0u8; 4]), 0);
        let run = |font: &FontData, x: f32, id: u32| GlyphRun {
            font: font.clone(),
            font_size: 12.0,
            normalized_coords: Arc::new([]),
            glyphs: Arc::new([Glyph { id, x, y: 0.0 }]),
        };
        let glyphs = |font: &FontData, x: f32, id: u32| {
            let mut list = DisplayList::default();
            list.glyphs(&run(font, x, id), Affine::IDENTITY, Color::WHITE);
            list
        };
        assert_eq!(close(&glyphs(&font, 5.0, 1), &glyphs(&font, 5.0 + 1e-6, 1)), None);
        assert!(close(&glyphs(&font, 5.0, 1), &glyphs(&font, 6.0, 1)).is_some());
        assert!(close(&glyphs(&font, 5.0, 1), &glyphs(&font, 5.0, 2)).is_some());
        let other_font = FontData::new(peniko::Blob::from(vec![0u8; 4]), 0);
        assert!(close(&glyphs(&font, 5.0, 1), &glyphs(&other_font, 5.0, 1)).is_some(), "another font");
    }

    #[test]
    fn blur_angle_covers_half_a_refresh_period() {
        let mut frame = Frame::new(100, 100, 1.0, Color::BLACK, 0.01);
        assert_eq!(frame.blur_angle(), 0.0);
        frame.request_motion_blur(Point::new(50.0, 50.0), 40.0, 20.0);
        assert!((frame.blur_angle() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn a_frame_knows_its_viewport() {
        let viewport = Frame::new(2560, 1600, 1.667, Color::BLACK, 0.01).viewport();
        assert_eq!(viewport, Viewport { width: 2560, height: 1600, scale: 1.667 });
        assert_eq!(viewport.size(), Size::new(2560.0, 1600.0));
    }

    #[test]
    fn finite_frames_pass_and_each_kind_of_nan_is_named() {
        let mut picture = DisplayList::default();
        picture.fill(disk(), Affine::IDENTITY, Color::WHITE);
        let picture = Picture::new(picture);
        let mut list = DisplayList::default();
        list.picture(&picture, Affine::rotate(1.0));
        list.stroke(disk(), Affine::IDENTITY, Stroke::new(2.0), Color::WHITE);
        assert_eq!(frame_with(list.clone()).non_finite(), None);

        let mut bad_path = disk();
        bad_path.line_to((f64::NAN, 0.0));
        let mut fill = list.clone();
        fill.fill(bad_path, Affine::IDENTITY, Color::WHITE);
        assert_eq!(frame_with(fill).non_finite(), Some("fill"));

        let mut stroke = list.clone();
        stroke.stroke(disk(), Affine::IDENTITY, Stroke::new(f64::INFINITY), Color::WHITE);
        assert_eq!(frame_with(stroke).non_finite(), Some("stroke"));

        let mut nested = DisplayList::default();
        nested.fill(disk(), Affine::scale(f64::NAN), Color::WHITE);
        let mut outer = list.clone();
        outer.picture(&Picture::new(nested), Affine::IDENTITY);
        assert_eq!(frame_with(outer).non_finite(), Some("fill"));

        let mut moved = list.clone();
        moved.picture(&picture, Affine::rotate(f64::NAN));
        assert_eq!(frame_with(moved).non_finite(), Some("picture transform"));

        let mut blurred = frame_with(list.clone());
        blurred.request_motion_blur(Point::new(1.0, 1.0), 10.0, f64::NAN);
        assert_eq!(blurred.non_finite(), Some("motion blur"));

        let mut top = frame_with(list);
        top.top.fill(disk(), Affine::IDENTITY, Color::new([f32::NAN, 0.0, 0.0, 1.0]));
        assert_eq!(top.non_finite(), Some("fill"));
    }

    #[test]
    fn a_nan_anywhere_in_a_path_or_glyph_run_is_found() {
        let p = Point::new(1.0, 2.0);
        let nan = Point::new(f64::NAN, 2.0);
        let elements = [
            PathEl::MoveTo(nan),
            PathEl::LineTo(nan),
            PathEl::QuadTo(nan, p),
            PathEl::QuadTo(p, nan),
            PathEl::CurveTo(nan, p, p),
            PathEl::CurveTo(p, nan, p),
            PathEl::CurveTo(p, p, nan),
        ];
        for element in elements {
            let mut path = BezPath::new();
            path.move_to(p);
            path.push(element);
            let mut list = DisplayList::default();
            list.fill(path, Affine::IDENTITY, Color::WHITE);
            assert_eq!(frame_with(list).non_finite(), Some("fill"), "{element:?}");
        }

        let run = |x: f32| GlyphRun {
            font: FontData::new(peniko::Blob::from(Vec::new()), 0),
            font_size: 12.0,
            normalized_coords: Arc::new([]),
            glyphs: Arc::new([Glyph { id: 1, x: 0.0, y: 0.0 }, Glyph { id: 2, x, y: 0.0 }]),
        };
        let mut list = DisplayList::default();
        list.glyphs(&run(5.0), Affine::IDENTITY, Color::WHITE);
        assert_eq!(frame_with(list.clone()).non_finite(), None);
        list.glyphs(&run(f32::NAN), Affine::IDENTITY, Color::WHITE);
        assert_eq!(frame_with(list).non_finite(), Some("glyphs"));
    }
}
