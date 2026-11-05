//! Text shaping and measuring with Parley: font fallback for emoji and complex
//! scripts, and truncation with an ellipsis. Lines come out as glyph runs for `Frame`.
//!
//! Font stacks name their families and never a generic one: on Linux a generic family
//! expands to fontconfig's whole sorted list (50+ families), which outranks the script
//! fallbacks and picks monochrome Symbola emoji or FreeSans Devanagari. Script fallbacks
//! are pinned to Noto for the same reason.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::Context as _;
use parley::fontique::{Blob, Collection, CollectionOptions, FallbackKey, Script, SourceCache};
use parley::{
    FontContext, FontFamily, FontFamilyName, FontWeight, GenericFamily, Layout, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};

use crate::frame::{Glyph, GlyphRun};

const ELLIPSIS: &str = "…";

/// Families that take a script over from fontconfig's pick, when they are installed.
/// The CJK ones are system fonts (too large to bundle).
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

/// Shaped lines each style's cache keeps. Wheel labels are shaped again only when the wheel
/// is rebuilt; overlay lines change almost every frame, so they get a small cache of their
/// own and never push labels out.
const LABEL_CACHE_LINES: usize = 4096;
const MONO_CACHE_LINES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextStyle {
    /// Wheel labels: bold sans-serif.
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

/// One shaped line. Glyph positions are relative to the start of the line (x = 0)
/// and to its baseline (y = 0), y pointing down.
#[derive(Clone, Debug, Default)]
pub struct TextLine {
    pub runs: Vec<GlyphRun>,
    /// Advance width, in pixels.
    pub width: f32,
    /// Distance from the baseline to the top of the line box, positive.
    pub ascent: f32,
    /// Distance from the baseline to the bottom of the line box, positive.
    pub descent: f32,
}

/// Shapes and caches lines of text. Font loading happens once, in `new`.
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
    /// System fonts, plus the font files given (which take precedence).
    pub fn new(extra_fonts: &[PathBuf]) -> anyhow::Result<Self> {
        let mut fonts = FontContext {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
            // Parley prunes this cache on every layout. Shared, it hands an evicted system
            // font back with the same Blob while anything (a cached line, a picture) still
            // holds it, and both renderers key their glyph caches by the Blob id.
            source_cache: SourceCache::new_shared(),
        };
        for path in extra_fonts {
            // Loaded into memory, the Blob (and its id) lives as long as the collection.
            let data =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let families = fonts.collection.register_fonts(Blob::from(data), None);
            anyhow::ensure!(!families.is_empty(), "no font in {}", path.display());
        }
        for (script, family) in SCRIPT_FALLBACKS {
            if let Some(id) = fonts.collection.family_id(family) {
                let key = FallbackKey::new(Script::from_bytes(*script), None);
                fonts.collection.set_fallbacks(key, std::iter::once(id));
            }
        }
        // Without the fonts folder (other OSes), generic families keep text visible.
        let mut named_or = |name: &'static str, generic| match fonts.collection.family_id(name) {
            Some(_) => FontFamilyName::named(name),
            None => FontFamilyName::Generic(generic),
        };
        let label_families = vec![
            named_or("Noto Sans", GenericFamily::SansSerif),
            named_or("Noto Color Emoji", GenericFamily::Emoji),
        ];
        let mono_families = vec![named_or("Noto Sans Mono", GenericFamily::Monospace)];
        Ok(Self {
            fonts,
            layouts: LayoutContext::new(),
            layout: Layout::default(),
            label_families,
            mono_families,
            label_cache: LineCache::new(LABEL_CACHE_LINES),
            mono_cache: LineCache::new(MONO_CACHE_LINES),
        })
    }

    /// Shapes `text` on a single line at `size` pixels, without wrapping. Tabs, line breaks
    /// and other control characters are shaped as spaces (see `one_line`).
    pub fn line(&mut self, text: &str, size: f32, style: TextStyle) -> TextLine {
        let key = (text.to_owned(), size.to_bits());
        if let Some(line) = self.cache(style).get(&key) {
            return line;
        }
        self.shape(&one_line(text), size, style);
        let line = first_line(&self.layout);
        self.cache(style).insert(key, line.clone());
        line
    }

    /// Like `line`, but cut at a cluster boundary and ended with "…" so the width
    /// stays within `max_width`. `None` when not even the ellipsis fits.
    pub fn line_fitting(
        &mut self,
        text: &str,
        size: f32,
        style: TextStyle,
        max_width: f32,
    ) -> Option<TextLine> {
        let fitted = self.fit(text, size, style, max_width)?;
        Some(self.line(&fitted, size, style))
    }

    /// The text `line_fitting` shapes: `text` made one line when it fits, else a prefix of
    /// that cut at a cluster boundary, trailing whitespace trimmed, with "…" after it.
    fn fit(&mut self, text: &str, size: f32, style: TextStyle, max_width: f32) -> Option<String> {
        // Made one line first: cluster offsets index the shaped text, and the characters
        // `one_line` replaces are not all one byte long.
        let text = one_line(text);
        if self.line(&text, size, style).width <= max_width {
            return Some(text.into_owned());
        }
        let ellipsis = self.line(ELLIPSIS, size, style).width;
        if ellipsis > max_width {
            return None;
        }
        // Shaped again for its clusters (a cached line keeps only glyphs), sorted into
        // logical order: a line holds its runs in visual order.
        self.shape(&text, size, style);
        let mut clusters: Vec<(usize, f32)> = self
            .layout
            .lines()
            .flat_map(|line| line.runs())
            .flat_map(|run| run.clusters())
            .map(|cluster| (cluster.text_range().start, cluster.advance()))
            .collect();
        clusters.sort_unstable_by_key(|&(start, _)| start);
        // Keep the clusters that fit beside the ellipsis, then give one back while the
        // shorter text, reshaped, still overflows (Arabic joining changes widths).
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
            let candidate = format!("{}{ELLIPSIS}", text[..cut].trim_end());
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

    /// Lays `text` out into `self.layout` on one line that never wraps. `text` must hold
    /// no line break: pass it through `one_line` first.
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
        debug_assert_eq!(self.layout.len(), 1, "{text:?} is not one line");
    }
}

/// `text` with its control characters (tab, CR, LF, VT, FF, NEL...) and the line and
/// paragraph separators replaced by spaces. Entries pasted or imported from TSV or CRLF
/// files carry them; Parley ends a line at most of them, and fonts have no glyph for the
/// rest (a tab shapes to .notdef).
fn one_line(text: &str) -> Cow<'_, str> {
    let breaks = |c: char| c.is_control() || c == '\u{2028}' || c == '\u{2029}';
    if text.contains(breaks) { Cow::Owned(text.replace(breaks, " ")) } else { Cow::Borrowed(text) }
}

/// The first line of `layout` (made from `one_line` text, its only one) as glyph runs.
/// Runs that share font, size and variation are merged (glyph positions are absolute),
/// so the stage issues fewer draw calls.
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
        // Font synthesis is not applied. Label fonts reach bold through their wght axis,
        // a variation already in these coords; only fonts without one (the colour emoji)
        // ask for faux bold, which would only smear them. No style asks for italic, so
        // no font asks for a skew.
        let coords: Vec<i16> = run.normalized_coords().iter().map(|c| c.to_bits()).collect();
        let glyphs = glyph_run.positioned_glyphs().map(|g| Glyph {
            id: g.id,
            x: g.x,
            y: g.y - metrics.baseline,
        });
        let same = runs.iter_mut().find(|r| {
            r.font == *run.font()
                && r.font_size == run.font_size()
                && *r.normalized_coords == *coords
        });
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
    // Parley lays empty text out as a space to get line metrics; without glyphs there
    // is no width. The layout's width is its widest line's, here this line's.
    let width = if runs.is_empty() { 0.0 } else { layout.width() };
    TextLine {
        runs,
        width,
        ascent: metrics.baseline - metrics.block_min_coord,
        descent: metrics.block_max_coord - metrics.baseline,
    }
}

/// Text and font size (its bits).
type LineKey = (String, u32);

/// One style's shaped lines, at most `max_lines` of them. Once full, each insert first drops
/// the oldest line, read or not: a frame that shapes new lines (the overlay's, almost every
/// frame) frees as many as it adds, one at a time, and the storage never grows.
struct LineCache {
    lines: HashMap<LineKey, TextLine>,
    /// The keys of `lines`, oldest first.
    order: VecDeque<LineKey>,
    max_lines: usize,
}

impl LineCache {
    fn new(max_lines: usize) -> Self {
        Self {
            // Room for twice the lines: removals leave tombstones, and once they take the
            // last free slots of a table over half full, hashbrown doubles it (measured
            // 0.4-0.9 ms at 4096 lines). Under half full it cleans them in place instead,
            // rarely: every ~100,000 inserts at 64 lines (3 µs), ~3 million at 4096 (0.2 ms).
            lines: HashMap::with_capacity(2 * max_lines),
            order: VecDeque::with_capacity(max_lines),
            max_lines,
        }
    }

    fn get(&self, key: &LineKey) -> Option<TextLine> {
        self.lines.get(key).cloned()
    }

    /// Only after `get` missed `key`, so `order` never holds a key twice.
    fn insert(&mut self, key: LineKey, line: TextLine) {
        debug_assert!(!self.lines.contains_key(&key), "{key:?} is already cached");
        if self.lines.len() == self.max_lines {
            let oldest = self.order.pop_front().expect("a full cache has an oldest line");
            self.lines.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.lines.insert(key, line);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use parley::FontData;
    use parley::fontique::SourceKind;

    use super::*;
    use crate::labels::{LabelSet, entries};

    /// An engine with every font file of the spike's fonts folder, as main.rs loads it.
    fn engine() -> TextEngine {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("the fonts folder")
            .map(|entry| entry.expect("a readable folder").path())
            .filter(|path| {
                let ext = path.extension().and_then(|e| e.to_str());
                matches!(ext, Some("ttf" | "otf" | "ttc"))
            })
            .collect();
        TextEngine::new(&files).expect("the bundled fonts load")
    }

    /// The registered (bundled) font of family `name`, not a system copy.
    fn bundled_font(text: &mut TextEngine, name: &str) -> FontData {
        let family = text.fonts.collection.family_by_name(name).expect(name);
        let font = &family.fonts()[0];
        let SourceKind::Memory(blob) = &font.source().kind else {
            panic!("{name} resolved to a system font");
        };
        FontData::new(blob.clone(), font.index())
    }

    fn glyphs(line: &TextLine) -> impl Iterator<Item = &Glyph> {
        line.runs.iter().flat_map(|run| run.glyphs.iter())
    }

    /// Han, kana and hangul come from system fonts, which the folder does not bundle.
    fn needs_cjk(text: &str) -> bool {
        let ranges = [
            '\u{1100}'..='\u{11FF}', // hangul jamo
            '\u{3040}'..='\u{30FF}', // kana
            '\u{3400}'..='\u{9FFF}', // han
            '\u{AC00}'..='\u{D7AF}', // hangul syllables
        ];
        text.chars().any(|c| ranges.iter().any(|range| range.contains(&c)))
    }

    #[test]
    fn sample_labels_have_no_missing_glyphs() {
        let mut text = engine();
        let labels =
            entries(LabelSet::Scripts, 100).into_iter().chain(entries(LabelSet::Names, 100));
        for label in labels.filter(|label| !needs_cjk(label)) {
            let line = text.line(&label, 24.0, TextStyle::Label);
            assert!(glyphs(&line).count() > 0, "{label}");
            assert!(glyphs(&line).all(|g| g.id != 0), "notdef in {label}");
        }
    }

    #[test]
    fn emoji_come_from_noto_color_emoji() {
        let mut text = engine();
        let emoji = bundled_font(&mut text, "Noto Color Emoji");
        let sans = bundled_font(&mut text, "Noto Sans");
        let line = text.line("Ciao 🎉", 24.0, TextStyle::Label);
        let fonts: Vec<(&FontData, usize)> =
            line.runs.iter().map(|r| (&r.font, r.glyphs.len())).collect();
        assert_eq!(fonts, [(&sans, 5), (&emoji, 1)]);
    }

    #[test]
    fn labels_are_bold_through_the_weight_axis() {
        let mut text = engine();
        let sans = bundled_font(&mut text, "Noto Sans");
        let line = text.line("Alice", 24.0, TextStyle::Label);
        assert_eq!(line.runs.len(), 1);
        assert_eq!(line.runs[0].font, sans);
        assert!(line.runs[0].normalized_coords.iter().any(|&c| c > 0), "wght above its default");
    }

    #[test]
    fn rtl_lines_start_at_zero_with_positive_width() {
        let mut text = engine();
        for label in ["שלום", "مرحبا بالعالم", "ﻻ 123 عربي"] {
            let line = text.line(label, 24.0, TextStyle::Label);
            assert!(line.width > 0.0, "{label}");
            let left = glyphs(&line).map(|g| g.x).fold(f32::INFINITY, f32::min);
            assert!(left.abs() < 1e-3, "{label} starts at {left}");
        }
    }

    #[test]
    fn mono_lines_use_a_monospaced_font() {
        let mut text = engine();
        let narrow = text.line("iiii", 12.0, TextStyle::Mono).width;
        let wide = text.line("WWWW", 12.0, TextStyle::Mono).width;
        assert!(narrow > 0.0 && (narrow - wide).abs() < 1e-3, "{narrow} against {wide}");
    }

    #[test]
    fn empty_text_has_line_metrics_but_no_width() {
        let mut text = engine();
        let line = text.line("", 24.0, TextStyle::Label);
        assert!(line.runs.is_empty() && line.width == 0.0, "{line:?}");
        assert!(line.ascent > 0.0 && line.descent > 0.0, "{line:?}");
    }

    #[test]
    fn ellipsis_stays_within_max_width() {
        let mut text = engine();
        let ellipsis = text.line(ELLIPSIS, 24.0, TextStyle::Label).width;
        let labels = [
            "Pierfrancesco Massimiliano",
            "مرحبا بالعالم",
            "שלום עולם",
            "👩‍👩‍👧‍👦 famiglia",
            "नमस्ते दुनिया",
            "ক্ষ বাংলা",
        ];
        for label in labels {
            let full = text.line(label, 24.0, TextStyle::Label).width;
            for step in 0..=60 {
                let max_width = full * step as f32 / 50.0;
                let fitted = text.fit(label, 24.0, TextStyle::Label, max_width);
                let line = text.line_fitting(label, 24.0, TextStyle::Label, max_width);
                match fitted {
                    None => {
                        assert!(max_width < ellipsis && line.is_none(), "{label} at {max_width}");
                    }
                    Some(fitted) if max_width >= full => assert_eq!(fitted, label),
                    Some(fitted) => {
                        let head = fitted.strip_suffix(ELLIPSIS).expect("ends with the ellipsis");
                        assert!(label.starts_with(head) && head == head.trim_end(), "{fitted}");
                        let width = line.expect("a line").width;
                        assert!(width <= max_width, "{fitted}: {width} > {max_width}");
                    }
                }
            }
        }
    }

    #[test]
    fn tabs_and_line_breaks_shape_as_spaces() {
        let mut text = engine();
        let ellipsis = text.line(ELLIPSIS, 24.0, TextStyle::Label).width;
        // An entry as pasted or imported (TSV, CRLF), and the text it shows.
        let cases = [
            ("a\tb", "a b"),
            ("a\nb", "a b"),
            ("a\u{2028}b", "a b"),
            ("Alice\tRossi\r\nBob\u{b}Carol\u{c}Dan\u{85}Eve\u{2029}Fay", "Alice Rossi  Bob Carol Dan Eve Fay"),
        ];
        let placed = |line: &TextLine| glyphs(line).map(|g| (g.id, g.x, g.y)).collect::<Vec<_>>();
        for (raw, visible) in cases {
            let line = text.line(raw, 24.0, TextStyle::Label);
            assert_eq!(text.layout.len(), 1, "{raw:?} laid out on several lines");
            assert!(glyphs(&line).all(|g| g.id != 0), "notdef in {raw:?}");
            let shown = text.line(visible, 24.0, TextStyle::Label);
            assert_eq!(placed(&line), placed(&shown), "{raw:?}");
            assert_eq!(line.width, shown.width, "{raw:?}");
            for step in 0..=60 {
                let max_width = line.width * step as f32 / 50.0;
                let Some(fitted) = text.fit(raw, 24.0, TextStyle::Label, max_width) else {
                    assert!(max_width < ellipsis, "{raw:?} at {max_width}");
                    continue;
                };
                let head = fitted.strip_suffix(ELLIPSIS).unwrap_or(&fitted);
                assert!(visible.starts_with(head), "{fitted:?}");
                let fitted_line = text.line_fitting(raw, 24.0, TextStyle::Label, max_width);
                let width = fitted_line.expect("a line").width;
                assert!(width <= max_width, "{fitted:?}: {width} > {max_width}");
            }
        }
    }

    #[test]
    fn cache_drops_its_oldest_line_once_full() {
        let key = |i: usize| (i.to_string(), 0);
        let mut cache = LineCache::new(3);
        for i in 0..3 {
            cache.insert(key(i), TextLine::default());
        }
        // The oldest line goes first, read or not.
        assert!(cache.get(&key(0)).is_some());
        cache.insert(key(3), TextLine::default());
        assert!(cache.get(&key(0)).is_none());
        assert!((1..4).all(|i| cache.get(&key(i)).is_some()));
    }

    #[test]
    fn cache_never_grows_under_churn() {
        let mut cache = LineCache::new(MONO_CACHE_LINES);
        let (map_room, queue_room) = (cache.lines.capacity(), cache.order.capacity());
        for i in 0..100 * MONO_CACHE_LINES {
            cache.insert((i.to_string(), 0), TextLine::default());
            assert!(cache.lines.len() <= MONO_CACHE_LINES && cache.order.len() == cache.lines.len());
            // Neither reallocates. A map's capacity() is its lines plus the free slots that
            // tombstones have not taken, so it rises past its start only if the table grows.
            assert!(cache.lines.capacity() <= map_room && cache.order.capacity() == queue_room);
        }
    }

    #[test]
    fn overlay_churn_does_not_evict_wheel_labels() {
        let mut text = engine();
        let labels = entries(LabelSet::Names, 20);
        for label in &labels {
            text.line(label, 24.0, TextStyle::Label);
        }
        for frame in 0..10 * MONO_CACHE_LINES {
            text.line(&format!("frames {frame} · interval 6.94 ms"), 12.0, TextStyle::Mono);
        }
        assert_eq!(text.mono_cache.lines.len(), MONO_CACHE_LINES);
        for label in &labels {
            let key = (label.clone(), 24f32.to_bits());
            assert!(text.label_cache.get(&key).is_some(), "{label} was dropped");
        }
    }
}
