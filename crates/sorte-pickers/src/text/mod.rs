//! Text shaping and measuring with Parley: font fallback for emoji and complex scripts, and
//! truncation with an ellipsis. Lines come out as glyph runs for `Frame`.
//!
//! Font stacks name their families and never a generic one: on Linux a generic family expands
//! to fontconfig's whole sorted list (50+ families), which outranks the script fallbacks and
//! picks monochrome Symbola emoji or FreeSans Devanagari. Script fallbacks are pinned to Noto
//! for the same reason.

mod cache;

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use parley::fontique::{Blob, Collection, CollectionOptions, FallbackKey, Script, SourceCache};
use parley::{
    FontContext, FontFamily, FontFamilyName, FontWeight, GenericFamily, Layout, LayoutContext, PositionedLayoutItem,
    StyleProperty,
};

use self::cache::LineCache;
use crate::frame::{Glyph, GlyphRun};

const ELLIPSIS: &str = "…";

/// Families that take a script over from the system's pick, when they are installed. The CJK
/// ones are system fonts (too large to bundle).
const SCRIPT_FALLBACKS: [(&[u8; 4], &str); 9] = [
    (b"Arab", "Noto Sans Arabic"),
    (b"Hebr", "Noto Sans Hebrew"),
    (b"Deva", "Noto Sans Devanagari"),
    (b"Beng", "Noto Sans Bengali"),
    (b"Thai", "Noto Sans Thai"),
    (b"Hani", "Noto Sans CJK SC"),
    (b"Hira", "Noto Sans CJK JP"),
    (b"Kana", "Noto Sans CJK JP"),
    (b"Hang", "Noto Sans CJK KR"),
];

/// Shaped lines each style's cache keeps. Labels are shaped again only when a picker rebuilds its
/// scene; overlay lines change almost every frame, so they get a small cache of their own and
/// never push labels out.
const LABEL_CACHE_LINES: usize = 4096;
const MONO_CACHE_LINES: usize = 64;

/// Where a font file comes from.
#[derive(Clone, Debug)]
pub enum FontSource {
    /// The bytes of a font file embedded in the executable (`include_bytes!`), used in place.
    Embedded(&'static [u8]),
    /// The bytes of a font file already in memory.
    Bytes(Vec<u8>),
    /// A font file on disk, read once when the engine is created.
    File(PathBuf),
}

/// A font source the engine could not use.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("cannot read the font file {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The data is not a font Parley can read.
    #[error("font source {index} holds no font")]
    NoFont {
        /// Position of the source in the list given to [`TextEngine::new`].
        index: usize,
    },
}

/// How a line is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextStyle {
    /// Entry labels: bold sans-serif, with colour emoji.
    Label,
    /// Debug overlay: monospace.
    Mono,
}

impl TextStyle {
    fn weight(self) -> FontWeight {
        match self {
            Self::Label => FontWeight::BOLD,
            Self::Mono => FontWeight::NORMAL,
        }
    }
}

/// One shaped line. Glyph positions are relative to the start of the line (x = 0) and to its
/// baseline (y = 0), y pointing down.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLine {
    pub runs: Vec<GlyphRun>,
    /// Advance width, in pixels.
    pub width: f32,
    /// Distance from the baseline to the top of the line box, positive.
    pub ascent: f32,
    /// Distance from the baseline to the bottom of the line box, positive.
    pub descent: f32,
}

/// Shapes and caches single lines of text. Fonts load once, in [`TextEngine::new`].
pub struct TextEngine {
    fonts: FontContext,
    layouts: LayoutContext<()>,
    /// Every shaping lands here, so its buffers are allocated once.
    layout: Layout<()>,
    label_families: Vec<FontFamilyName<'static>>,
    mono_families: Vec<FontFamilyName<'static>>,
    label_cache: LineCache,
    mono_cache: LineCache,
}

impl TextEngine {
    /// An engine over the system's fonts plus `fonts`, which take precedence: the app passes the
    /// Noto families it embeds (Noto Sans, Noto Sans Mono, Noto Color Emoji and the script
    /// families of `SCRIPT_FALLBACKS`), so labels look the same on every OS. Without them text
    /// falls back to the system's generic families.
    pub fn new(fonts: impl IntoIterator<Item = FontSource>) -> Result<Self, FontError> {
        let mut context = FontContext {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
            // Parley prunes this cache on every layout. Shared, it hands an evicted system font
            // back with the same Blob while anything (a cached line, a picture) still holds it,
            // and both renderers key their glyph caches by the Blob id.
            source_cache: SourceCache::new_shared(),
        };
        for (index, source) in fonts.into_iter().enumerate() {
            // Held in memory, the Blob (and its id) lives as long as the collection.
            let blob = match source {
                FontSource::Embedded(bytes) => Blob::new(Arc::new(bytes)),
                FontSource::Bytes(bytes) => Blob::from(bytes),
                FontSource::File(path) => match std::fs::read(&path) {
                    Ok(bytes) => Blob::from(bytes),
                    Err(source) => return Err(FontError::Read { path, source }),
                },
            };
            if context.collection.register_fonts(blob, None).is_empty() {
                return Err(FontError::NoFont { index });
            }
        }
        for (script, family) in SCRIPT_FALLBACKS {
            if let Some(id) = context.collection.family_id(family) {
                let key = FallbackKey::new(Script::from_bytes(*script), None);
                context.collection.set_fallbacks(key, std::iter::once(id));
            }
        }
        // A missing named family would be dropped silently: generic ones keep text visible.
        let mut named_or = |name: &'static str, generic| match context.collection.family_id(name) {
            Some(_) => FontFamilyName::named(name),
            None => FontFamilyName::Generic(generic),
        };
        let label_families =
            vec![named_or("Noto Sans", GenericFamily::SansSerif), named_or("Noto Color Emoji", GenericFamily::Emoji)];
        let mono_families = vec![named_or("Noto Sans Mono", GenericFamily::Monospace)];
        Ok(Self {
            fonts: context,
            layouts: LayoutContext::new(),
            layout: Layout::default(),
            label_families,
            mono_families,
            label_cache: LineCache::new(LABEL_CACHE_LINES),
            mono_cache: LineCache::new(MONO_CACHE_LINES),
        })
    }

    /// Shapes `text` on a single line at `size` pixels, without wrapping. Tabs, line breaks and
    /// other control characters are shaped as spaces: entries pasted or imported from TSV or
    /// CRLF files carry them. A size that is not a positive number gives an empty line.
    pub fn line(&mut self, text: &str, size: f32, style: TextStyle) -> TextLine {
        if !valid_size(size) {
            return TextLine::default();
        }
        let key = (text.to_owned(), size.to_bits());
        if let Some(line) = self.cache(style).get(&key) {
            return line;
        }
        self.shape(&one_line(text), size, style);
        let line = first_line(&self.layout);
        self.cache(style).insert(key, line.clone());
        line
    }

    /// Like [`line`](Self::line), but cut at a cluster boundary and ended with "…" so the width
    /// stays within `max_width`. `None` when not even the ellipsis fits, or when `size` is not a
    /// positive number.
    pub fn line_fitting(&mut self, text: &str, size: f32, style: TextStyle, max_width: f32) -> Option<TextLine> {
        let fitted = self.fit(text, size, style, max_width)?;
        Some(self.line(&fitted, size, style))
    }

    /// The text `line_fitting` shapes: `text` made one line when it fits, else a prefix of that
    /// cut at a cluster boundary, trailing whitespace trimmed, with "…" after it.
    fn fit(&mut self, text: &str, size: f32, style: TextStyle, max_width: f32) -> Option<String> {
        if !valid_size(size) {
            return None;
        }
        // Made one line first: cluster offsets index the shaped text, and the characters
        // `one_line` replaces are not all one byte long.
        let text = one_line(text);
        if self.line(&text, size, style).width <= max_width {
            return Some(text.into_owned());
        }
        let ellipsis = self.line(ELLIPSIS, size, style).width;
        if max_width.is_nan() || ellipsis > max_width {
            return None;
        }
        // Shaped again for its clusters (a cached line keeps only glyphs), sorted into logical
        // order: a line holds its runs in visual order.
        self.shape(&text, size, style);
        let mut clusters: Vec<(usize, f32)> = self
            .layout
            .lines()
            .flat_map(|line| line.runs())
            .flat_map(|run| run.clusters())
            .map(|cluster| (cluster.text_range().start, cluster.advance()))
            .collect();
        clusters.sort_unstable_by_key(|&(start, _)| start);
        // Keep the clusters that fit beside the ellipsis, then give one back while the shorter
        // text, reshaped, still overflows (Arabic joining changes widths).
        let mut room = max_width - ellipsis;
        let mut kept = clusters
            .iter()
            .take_while(|&&(_, advance)| {
                room -= advance;
                room >= 0.0
            })
            .count();
        loop {
            let cut = clusters.get(kept).map_or(text.len(), |&(start, _)| start);
            let head = text.get(..cut).unwrap_or_default();
            let candidate = format!("{}{ELLIPSIS}", head.trim_end());
            if kept == 0 || self.line(&candidate, size, style).width <= max_width {
                return Some(candidate);
            }
            kept -= 1;
        }
    }

    fn cache(&mut self, style: TextStyle) -> &mut LineCache {
        match style {
            TextStyle::Label => &mut self.label_cache,
            TextStyle::Mono => &mut self.mono_cache,
        }
    }

    /// Lays `text` out into `self.layout` on one line that never wraps. `text` must hold no line
    /// break: pass it through `one_line` first.
    fn shape(&mut self, text: &str, size: f32, style: TextStyle) {
        let families = match style {
            TextStyle::Label => &self.label_families,
            TextStyle::Mono => &self.mono_families,
        };
        // Scale 1 and no quantizing: the stage's transform carries DPI and rotation, and
        // positions stay exact under it.
        let mut builder = self.layouts.ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(FontFamily::List(Cow::Borrowed(families)));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(style.weight());
        builder.build_into(&mut self.layout, text);
        self.layout.break_all_lines(None);
    }
}

impl fmt::Debug for TextEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextEngine")
            .field("label_lines", &self.label_cache.len())
            .field("mono_lines", &self.mono_cache.len())
            .finish_non_exhaustive()
    }
}

fn valid_size(size: f32) -> bool {
    size.is_finite() && size > 0.0
}

/// `text` with its control characters (tab, CR, LF, VT, FF, NEL...) and the line and paragraph
/// separators replaced by spaces. Parley ends a line at most of them, and fonts have no glyph
/// for the rest (a tab shapes to .notdef).
fn one_line(text: &str) -> Cow<'_, str> {
    let breaks = |c: char| c.is_control() || c == '\u{2028}' || c == '\u{2029}';
    if text.contains(breaks) { Cow::Owned(text.replace(breaks, " ")) } else { Cow::Borrowed(text) }
}

/// The first line of `layout` (made from `one_line` text, its only one) as glyph runs. Runs that
/// share font, size and variation are merged (glyph positions are absolute), so the stage
/// issues fewer draw calls.
fn first_line(layout: &Layout<()>) -> TextLine {
    let Some(line) = layout.lines().next() else {
        return TextLine::default();
    };
    let metrics = line.metrics();
    let mut runs: Vec<GlyphRun> = Vec::new();
    for item in line.items() {
        let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
            continue;
        };
        let run = glyph_run.run();
        // Font synthesis is not applied. Label fonts reach bold through their wght axis, a
        // variation already in these coords; only fonts without one (the colour emoji) ask for
        // faux bold, which would only smear them. No style asks for italic, so no font asks for
        // a skew.
        let coords: Vec<i16> = run.normalized_coords().iter().map(|c| c.to_bits()).collect();
        let glyphs = glyph_run.positioned_glyphs().map(|g| Glyph { id: g.id, x: g.x, y: g.y - metrics.baseline });
        let same = runs
            .iter_mut()
            .find(|r| r.font == *run.font() && r.font_size == run.font_size() && *r.normalized_coords == *coords);
        match same {
            Some(same) => same.glyphs = same.glyphs.iter().copied().chain(glyphs).collect(),
            None => runs.push(GlyphRun {
                font: run.font().clone(),
                font_size: run.font_size(),
                normalized_coords: coords.into(),
                glyphs: glyphs.collect(),
            }),
        }
    }
    // Parley lays empty text out as a space to get line metrics; without glyphs there is no
    // width. The layout's width is its widest line's, here this line's.
    let width = if runs.is_empty() { 0.0 } else { layout.width() };
    TextLine {
        runs,
        width,
        ascent: metrics.baseline - metrics.block_min_coord,
        descent: metrics.block_max_coord - metrics.baseline,
    }
}

#[cfg(test)]
mod tests;
