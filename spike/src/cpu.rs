//! CPU fallback renderer: replays a `Frame` with vello_cpu into premultiplied RGBA8.
//! Motion blur is skipped here (CLAUDE.md: it is a GPU pass only).

use std::time::{Duration, Instant};

use vello::kurbo::Affine;
use vello::peniko::Fill;
use vello_cpu::{
    Glyph, Level, PixmapMut, RasterizerSettings, RenderContext, RenderSettings, Resources, TargetInit,
};

use crate::frame::{Cmd, Frame};

/// What the last `render` cost, by phase.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuTiming {
    /// Replaying the display lists on the calling thread. Glyphs are always prepared
    /// here; without workers, paths are also turned into strips here.
    pub encode_ms: f32,
    /// Waiting for the workers to finish their strips (0 without workers).
    pub flush_ms: f32,
    /// Rasterizing into the target (on the workers, when there are any).
    pub render_ms: f32,
}

pub struct CpuStage {
    /// Owns the worker pool and keeps its allocations across frames: made once.
    ctx: RenderContext,
    /// Glyph outline caches, keyed by font blob: one per context, kept as long.
    resources: Resources,
    /// Glyphs that cannot be drawn are reported once, not at every frame.
    glyph_error_reported: bool,
    pub timing: CpuTiming,
}

impl CpuStage {
    /// `threads`: vello_cpu worker threads (0 = single-threaded). Capped at the machine's
    /// parallelism, and at 255: vello_cpu numbers its workers with a u8 and panics past it.
    pub fn new(threads: u16) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = threads.min(u16::try_from(cores).unwrap_or(u16::MAX)).min(255);
        let settings =
            RenderSettings { level: Level::try_detect().unwrap_or(Level::baseline()), num_threads: threads };
        Self {
            // The real size comes with the first frame.
            ctx: RenderContext::new_with(1, 1, settings),
            resources: Resources::new(),
            glyph_error_reported: false,
            timing: CpuTiming::default(),
        }
    }

    /// Draws `frame` into `target`: the background, then `moving`, then `top`.
    /// `target` holds frame.width × frame.height premultiplied RGBA8 pixels, tightly
    /// packed, row after row; its previous content is overwritten.
    ///
    /// A frame with a side of 0 or above MAX_SIDE pixels is not drawn: vello_cpu sizes are
    /// u16, and it already overflows near the top of that range. Panics if `target` has
    /// the wrong length.
    pub fn render(&mut self, frame: &Frame, target: &mut [u8]) {
        const MAX_SIDE: u32 = 16384;
        if frame.width == 0 || frame.height == 0 || frame.width > MAX_SIDE || frame.height > MAX_SIDE {
            return;
        }
        let (width, height) = (frame.width as u16, frame.height as u16);
        let target = PixmapMut::new(width, height, target)
            .expect("the target holds frame.width × frame.height RGBA8 pixels");

        let start = Instant::now();
        if (self.ctx.width(), self.ctx.height()) == (width, height) {
            self.ctx.reset();
        } else {
            self.ctx.reset_and_resize(width, height);
        }
        // reset() restores the default state; the GPU path fills with NonZero too.
        self.ctx.set_fill_rule(Fill::NonZero);
        self.replay(frame.moving.cmds(), Affine::IDENTITY);
        self.replay(frame.top.cmds(), Affine::IDENTITY);
        let encoded = Instant::now();

        // Waits for the workers; a no-op when single-threaded.
        self.ctx.flush();
        let flushed = Instant::now();

        // Clearing to the background costs nothing extra: the rasterizer initializes
        // every pixel no opaque fill covers, with this colour instead of transparent.
        let settings = RasterizerSettings {
            target_init: TargetInit::Clear(frame.background),
            ..RasterizerSettings::default()
        };
        self.ctx.render_with(target, &mut self.resources, settings);

        self.timing = CpuTiming {
            encode_ms: ms(encoded - start),
            flush_ms: ms(flushed - encoded),
            render_ms: ms(flushed.elapsed()),
        };
    }

    /// Replays `cmds` under `parent`: each command's transform applies first, then the
    /// parent's (parent * child), as `Scene::append` composes on the GPU path.
    fn replay(&mut self, cmds: &[Cmd], parent: Affine) {
        for cmd in cmds {
            match cmd {
                Cmd::Fill { path, transform, color } => {
                    self.ctx.set_transform(parent * *transform);
                    self.ctx.set_paint(*color);
                    self.ctx.fill_path(path);
                }
                Cmd::Stroke { path, transform, style, color } => {
                    self.ctx.set_transform(parent * *transform);
                    self.ctx.set_paint(*color);
                    self.ctx.set_stroke((**style).clone());
                    self.ctx.stroke_path(path);
                }
                Cmd::Glyphs { run, transform, color } => {
                    // glyph_run() captures the transform and the paint: set them first.
                    self.ctx.set_transform(parent * *transform);
                    self.ctx.set_paint(*color);
                    let drawn = self
                        .ctx
                        .glyph_run(&mut self.resources, &run.font)
                        .font_size(run.font_size)
                        // vello_cpu hints by default and Vello GPU does not: match the GPU.
                        .hint(false)
                        .normalized_coords(&run.normalized_coords)
                        .fill_glyphs(run.glyphs.iter().map(|g| Glyph { id: g.id, x: g.x, y: g.y }));
                    // The other glyphs of the run are drawn; the skipped ones stay blank.
                    if let Err(err) = drawn
                        && !self.glyph_error_reported
                    {
                        self.glyph_error_reported = true;
                        eprintln!("vello_cpu: {err} (reported once)");
                    }
                }
                Cmd::Picture { picture, transform } => self.replay(picture.cmds(), parent * *transform),
            }
        }
    }
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;
    use std::sync::Arc;

    use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
    use parley::{FontContext, FontFamily, LayoutContext, PositionedLayoutItem, StyleProperty};
    use vello::kurbo::{Circle, CircleSegment, Rect, Shape, Stroke};
    use vello::peniko::Color;

    use super::*;
    use crate::frame::{self, DisplayList, GlyphRun, Picture};

    const BACKGROUND: Color = Color::from_rgb8(10, 20, 30);
    const SLICE: Color = Color::from_rgb8(200, 40, 40);
    const RIM: Color = Color::WHITE;
    const TOP: Color = Color::from_rgb8(0, 200, 0);

    fn pixel(target: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        target[i..i + 4].try_into().unwrap()
    }

    fn rgba(c: Color) -> [u8; 4] {
        c.to_rgba8().to_u8_array()
    }

    fn render(stage: &mut CpuStage, frame: &Frame) -> Vec<u8> {
        let mut target = vec![0x55; (frame.width * frame.height * 4) as usize];
        stage.render(frame, &mut target);
        target
    }

    /// A quarter of a disc in a picture nested in another, a rim, and an opaque square
    /// in the top layer. Centre (32, 24), radius 20.
    fn wheel_frame(width: u32, height: u32) -> Frame {
        let mut frame = Frame::new(width, height, 1.0, BACKGROUND, 1.0 / 60.0);
        // Each level transforms: only parent * child, at every level, puts the slice
        // where the assertions expect it.
        let mut slice = DisplayList::default();
        let quarter = CircleSegment::new((0.0, 0.0), 10.0, 0.0, 0.0, FRAC_PI_2);
        slice.fill(quarter.to_path(0.1), Affine::scale(2.0), SLICE);
        let mut wheel = DisplayList::default();
        // +π/2 turns +x into +y: the quarter moves from bottom-right to bottom-left.
        wheel.picture(&Picture::new(slice), Affine::rotate(FRAC_PI_2));
        frame.moving.picture(&Picture::new(wheel), Affine::translate((32.0, 24.0)));
        let rim = Circle::new((32.0, 24.0), 20.0).to_path(0.1);
        frame.moving.stroke(rim, Affine::IDENTITY, Stroke::new(2.0), RIM);
        frame.top.fill(Rect::new(22.0, 36.0, 28.0, 42.0).to_path(0.1), Affine::IDENTITY, TOP);
        // Requested but ignored: the CPU path has no blur.
        frame.request_motion_blur((32.0, 24.0).into(), 20.0, 30.0);
        frame
    }

    #[test]
    fn draws_background_moving_then_top() {
        let frame = wheel_frame(64, 48);
        let out = render(&mut CpuStage::new(0), &frame);
        let at = |x, y| pixel(&out, 64, x, y);
        assert_eq!(at(0, 0), rgba(BACKGROUND));
        assert_eq!(at(63, 47), rgba(BACKGROUND));
        // Bottom-left quadrant, inside the slice; the other quadrants are background.
        assert_eq!(at(26, 30), rgba(SLICE));
        assert_eq!(at(38, 30), rgba(BACKGROUND));
        assert_eq!(at(38, 18), rgba(BACKGROUND));
        assert_eq!(at(26, 18), rgba(BACKGROUND));
        // The rim's 2 px band (radii 19..21) fully covers pixel (32, 4).
        assert_eq!(at(32, 4), rgba(RIM));
        // The top layer covers the slice.
        assert_eq!(at(24, 38), rgba(TOP));
    }

    #[test]
    fn output_is_premultiplied() {
        let mut frame = Frame::new(8, 8, 1.0, Color::TRANSPARENT, 1.0 / 60.0);
        let red = Color::from_rgba8(255, 0, 0, 128);
        frame.top.fill(Rect::new(0.0, 0.0, 4.0, 8.0).to_path(0.1), Affine::IDENTITY, red);
        let blue = Color::from_rgb8(0, 0, 255).with_alpha(0.25);
        frame.moving.fill(Rect::new(4.0, 0.0, 8.0, 8.0).to_path(0.1), Affine::IDENTITY, blue);
        let out = render(&mut CpuStage::new(0), &frame);
        // Straight alpha would read (255, 0, 0, 128) and (0, 0, 255, 64).
        assert_eq!(pixel(&out, 8, 1, 1), [128, 0, 0, 128]);
        assert_eq!(pixel(&out, 8, 6, 6), [0, 0, 64, 64]);
    }

    #[test]
    fn threads_change_nothing_but_speed() {
        let frame = wheel_frame(301, 203);
        let single = render(&mut CpuStage::new(0), &frame);
        let multi = render(&mut CpuStage::new(3), &frame);
        assert!(single == multi, "multi-threaded output differs");
    }

    #[test]
    fn follows_the_frame_size() {
        let mut stage = CpuStage::new(2);
        for (w, h) in [(64, 48), (37, 29), (64, 48), (1, 1)] {
            let out = render(&mut stage, &wheel_frame(w, h));
            assert_eq!(pixel(&out, w, w - 1, h - 1), rgba(BACKGROUND), "{w}x{h}");
            if w == 64 {
                assert_eq!(pixel(&out, w, 26, 30), rgba(SLICE));
            }
        }
        // Nothing to draw, and no panic.
        stage.render(&wheel_frame(0, 48), &mut []);
    }

    fn noto_sans() -> (FontContext, String) {
        // Only the spike's font: no system fonts, so the test is the same everywhere.
        let mut fcx = FontContext {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: false }),
            source_cache: SourceCache::default(),
        };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/NotoSans[wght].ttf");
        let bytes = std::fs::read(path).expect("the spike's Noto Sans");
        let families = fcx.collection.register_fonts(Blob::from(bytes), None);
        let name = fcx.collection.family_name(families[0].0).expect("a family name").to_owned();
        (fcx, name)
    }

    /// "H" shaped by Parley, its baseline origin at (0, 0).
    fn letter_h(size: f32) -> GlyphRun {
        let (mut fcx, family) = noto_sans();
        let mut lcx = LayoutContext::<()>::new();
        let mut builder = lcx.ranged_builder(&mut fcx, "H", 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family)));
        builder.push_default(StyleProperty::FontSize(size));
        let mut layout = builder.build("H");
        layout.break_all_lines(None);
        let line = layout.lines().next().expect("one line");
        let Some(PositionedLayoutItem::GlyphRun(run)) = line.items().next() else {
            panic!("a glyph run");
        };
        let glyphs: Vec<frame::Glyph> =
            run.glyphs().map(|g| frame::Glyph { id: g.id, x: 0.0, y: 0.0 }).collect();
        GlyphRun {
            font: run.run().font().clone(),
            font_size: run.run().font_size(),
            normalized_coords: run.run().normalized_coords().iter().map(|c| c.to_bits()).collect(),
            glyphs: glyphs.into(),
        }
    }

    /// Bounding box (x0, y0, x1, y1) of the pixels that differ from `background`.
    fn ink(out: &[u8], width: u32, background: [u8; 4]) -> (u32, u32, u32, u32) {
        let mut bbox = (u32::MAX, u32::MAX, 0, 0);
        for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
            if *px != background {
                let (x, y) = (i as u32 % width, i as u32 / width);
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
        }
        bbox
    }

    #[test]
    fn glyphs_follow_their_transform() {
        let h = letter_h(40.0);
        // Noto Sans "H" at 40 px spans x 3.9..25.7 and y -28.6..0 around its origin
        // (skrifa's outline bounds), so the ink covers these pixels, ±1 for coverage.
        let mut stage = CpuStage::new(0);
        let mut frame = Frame::new(100, 60, 1.0, Color::WHITE, 1.0 / 60.0);
        frame.top.glyphs(&h, Affine::translate((10.0, 50.0)), Color::BLACK);
        let (x0, y0, x1, y1) = ink(&render(&mut stage, &frame), 100, [255; 4]);
        assert!((12..=14).contains(&x0) && (34..=36).contains(&x1), "x {x0}..{x1}");
        assert!((20..=22).contains(&y0) && (48..=49).contains(&y1), "y {y0}..{y1}");

        // A quarter turn: the glyph's up (-y) becomes +x, its +x becomes +y.
        let mut frame = Frame::new(100, 60, 1.0, Color::WHITE, 1.0 / 60.0);
        frame.top.glyphs(&h, Affine::translate((60.0, 10.0)) * Affine::rotate(FRAC_PI_2), Color::BLACK);
        let (x0, y0, x1, y1) = ink(&render(&mut stage, &frame), 100, [255; 4]);
        assert!((59..=60).contains(&x0) && (87..=89).contains(&x1), "x {x0}..{x1}");
        assert!((12..=14).contains(&y0) && (34..=36).contains(&y1), "y {y0}..{y1}");
    }

    #[test]
    fn missing_glyphs_are_reported_not_fatal() {
        let mut run = letter_h(40.0);
        let h = run.glyphs[0];
        // A glyph id past the end of the font, then a real "H" further right.
        run.glyphs = Arc::from([frame::Glyph { id: 60_000, ..h }, frame::Glyph { x: 40.0, ..h }]);
        let mut frame = Frame::new(100, 60, 1.0, Color::WHITE, 1.0 / 60.0);
        frame.top.glyphs(&run, Affine::translate((10.0, 50.0)), Color::BLACK);
        let mut stage = CpuStage::new(0);
        let out = render(&mut stage, &frame);
        assert!(stage.glyph_error_reported);
        let (x0, _, x1, _) = ink(&out, 100, [255; 4]);
        assert!(x0 >= 50 && x1 <= 77, "only the second glyph is drawn: x {x0}..{x1}");
    }
}
