//! The drawing contract between pickers and the stage (`Frame` in the design doc).
//!
//! Pickers never talk to a renderer. They record drawing commands into display lists;
//! the stage replays them with Vello on the GPU or with vello_cpu. A `Picture` is a
//! display list prepared once (the wheel with its labels) and drawn every frame under
//! a transform, so the GPU backend can encode it once and reuse the encoding.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vello::kurbo::{Affine, BezPath, Point, Stroke};
use vello::peniko::{Color, FontData};

/// One glyph, positioned in the run's coordinate space (y down, baseline at y = 0 of its line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// Glyphs that share a font and size: what both renderers draw in one call.
#[derive(Clone, Debug)]
pub struct GlyphRun {
    pub font: FontData,
    pub font_size: f32,
    /// Variable-font coordinates (F2Dot14), empty for static fonts.
    pub normalized_coords: Arc<[i16]>,
    pub glyphs: Arc<[Glyph]>,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    Fill { path: Arc<BezPath>, transform: Affine, color: Color },
    Stroke { path: Arc<BezPath>, transform: Affine, style: Arc<Stroke>, color: Color },
    Glyphs { run: GlyphRun, transform: Affine, color: Color },
    Picture { picture: Arc<Picture>, transform: Affine },
}

/// An ordered list of drawing commands.
#[derive(Clone, Debug, Default)]
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

/// A display list recorded once and drawn many times. Its id changes whenever its
/// content does, so a backend may cache whatever it derives from it under that id.
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

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn cmds(&self) -> &[Cmd] {
        self.list.cmds()
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
/// `moving` holds what may be motion-blurred (the wheel), `top` what stays sharp
/// above it (pointer, hub, debug overlay). Backends that cannot blur draw `moving`
/// then `top` into the same target.
#[derive(Debug)]
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

    pub fn request_motion_blur(&mut self, center: Point, radius: f64, omega: f64) {
        self.motion_blur = Some(MotionBlur { center, radius, omega });
    }

    /// Arc swept during half a refresh period, in radians: the blur's shutter angle.
    pub fn blur_angle(&self) -> f64 {
        self.motion_blur.map_or(0.0, |b| b.omega * self.period * 0.5)
    }
}
