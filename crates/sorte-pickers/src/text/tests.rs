//! Shaping tests over the Noto fonts of the M0 spike (`spike/fonts/`, not in git). Those tests
//! skip when the folder is absent, as in CI; the ones that need no bundled font always run.

use std::path::{Path, PathBuf};

use parley::FontData;
use parley::fontique::SourceKind;

use super::*;

const NAMES: &[&str] = &[
    "Alice",
    "Bruno",
    "Carla",
    "Dario",
    "Elena",
    "Fabio",
    "Giulia",
    "Hugo",
    "Irene",
    "Luca",
    "Marta",
    "Nicola",
    "Olga",
    "Paolo",
    "Rita",
    "Sara",
    "Tommaso",
    "Ugo",
    "Vera",
    "Zeno",
    "Beatrice",
    "Massimiliano",
    "Ludovica",
    "Pierfrancesco",
];

/// Complex scripts, right-to-left text and colour emoji.
const SCRIPTS: &[&str] = &[
    "Ciao 🎉",
    "مرحبا بالعالم",
    "שלום",
    "नमस्ते",
    "你好",
    "こんにちは",
    "안녕하세요",
    "สวัสดี",
    "Γειά σου",
    "Привет",
    "🍕 Pizza",
    "😀😎🤖",
    "Zoë & Chloé",
    "ﻻ 123 عربي",
    "Xin chào",
    "👩‍👩‍👧‍👦 famiglia",
    "🇮🇹 Italia",
    "ক্ষ বাংলা",
];

fn fonts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spike/fonts")
}

/// The font files of the spike's folder, or `None` when it is absent or empty.
fn font_files() -> Option<Vec<PathBuf>> {
    let files: Vec<PathBuf> = std::fs::read_dir(fonts_dir())
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| matches!(path.extension().and_then(|e| e.to_str()), Some("ttf" | "otf" | "ttc")))
        .collect();
    (!files.is_empty()).then_some(files)
}

/// An engine with every font of the spike's folder, as the app will load its embedded ones, or
/// `None` without the folder: the calling test then skips.
fn engine() -> Option<TextEngine> {
    let Some(files) = font_files() else {
        eprintln!("skipped: no fonts in {}", fonts_dir().display());
        return None;
    };
    Some(TextEngine::new(files.into_iter().map(FontSource::File)).expect("the spike's fonts load"))
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
    let Some(mut text) = engine() else { return };
    let labels = SCRIPTS.iter().chain(NAMES).flat_map(|&base| [base.to_owned(), format!("{base} 2")]);
    for label in labels.filter(|label| !needs_cjk(label)) {
        let line = text.line(&label, 24.0, TextStyle::Label);
        assert!(glyphs(&line).count() > 0, "{label}");
        assert!(glyphs(&line).all(|g| g.id != 0), "notdef in {label}");
    }
}

#[test]
fn emoji_come_from_noto_color_emoji() {
    let Some(mut text) = engine() else { return };
    let emoji = bundled_font(&mut text, "Noto Color Emoji");
    let sans = bundled_font(&mut text, "Noto Sans");
    let line = text.line("Ciao 🎉", 24.0, TextStyle::Label);
    let fonts: Vec<(&FontData, usize)> = line.runs.iter().map(|r| (&r.font, r.glyphs.len())).collect();
    assert_eq!(fonts, [(&sans, 5), (&emoji, 1)]);
}

#[test]
fn labels_are_bold_through_the_weight_axis() {
    let Some(mut text) = engine() else { return };
    let sans = bundled_font(&mut text, "Noto Sans");
    let line = text.line("Alice", 24.0, TextStyle::Label);
    assert_eq!(line.runs.len(), 1);
    assert_eq!(line.runs[0].font, sans);
    assert!(line.runs[0].normalized_coords.iter().any(|&c| c > 0), "wght above its default");
}

#[test]
fn embedded_fonts_shape_like_the_same_file() {
    let Some(_) = font_files() else { return };
    let path = fonts_dir().join("NotoSans[wght].ttf");
    let bytes: &'static [u8] = Box::leak(std::fs::read(&path).expect("Noto Sans").into_boxed_slice());
    let mut embedded = TextEngine::new([FontSource::Embedded(bytes)]).expect("an embedded font loads");
    let mut file = TextEngine::new([FontSource::File(path)]).expect("a font file loads");
    let (a, b) = (embedded.line("Alice", 24.0, TextStyle::Label), file.line("Alice", 24.0, TextStyle::Label));
    assert_eq!(glyphs(&a).collect::<Vec<_>>(), glyphs(&b).collect::<Vec<_>>());
    assert_eq!((a.width, a.ascent, a.descent), (b.width, b.ascent, b.descent));
}

#[test]
fn rtl_lines_start_at_zero_with_positive_width() {
    let Some(mut text) = engine() else { return };
    for label in ["שלום", "مرحبا بالعالم", "ﻻ 123 عربي"] {
        let line = text.line(label, 24.0, TextStyle::Label);
        assert!(line.width > 0.0, "{label}");
        let left = glyphs(&line).map(|g| g.x).fold(f32::INFINITY, f32::min);
        assert!(left.abs() < 1e-3, "{label} starts at {left}");
    }
}

#[test]
fn mono_lines_use_a_monospaced_font() {
    let Some(mut text) = engine() else { return };
    let narrow = text.line("iiii", 12.0, TextStyle::Mono).width;
    let wide = text.line("WWWW", 12.0, TextStyle::Mono).width;
    assert!(narrow > 0.0 && (narrow - wide).abs() < 1e-3, "{narrow} against {wide}");
}

#[test]
fn empty_text_has_line_metrics_but_no_width() {
    let Some(mut text) = engine() else { return };
    let line = text.line("", 24.0, TextStyle::Label);
    assert!(line.runs.is_empty() && line.width == 0.0, "{line:?}");
    assert!(line.ascent > 0.0 && line.descent > 0.0, "{line:?}");
}

#[test]
fn ellipsis_stays_within_max_width() {
    let Some(mut text) = engine() else { return };
    let ellipsis = text.line(ELLIPSIS, 24.0, TextStyle::Label).width;
    let labels = ["Pierfrancesco Massimiliano", "مرحبا بالعالم", "שלום עולם", "👩‍👩‍👧‍👦 famiglia", "नमस्ते दुनिया", "ক্ষ বাংলা"];
    for label in labels {
        let full = text.line(label, 24.0, TextStyle::Label).width;
        assert!(text.line_fitting(label, 24.0, TextStyle::Label, f32::NAN).is_none(), "{label} fits a NaN width");
        for step in 0..=60 {
            let max_width = full * step as f32 / 50.0;
            let fitted = text.fit(label, 24.0, TextStyle::Label, max_width);
            let line = text.line_fitting(label, 24.0, TextStyle::Label, max_width);
            match fitted {
                None => assert!(max_width < ellipsis && line.is_none(), "{label} at {max_width}"),
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
    let Some(mut text) = engine() else { return };
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
fn overlay_churn_does_not_evict_labels() {
    let Some(mut text) = engine() else { return };
    for label in NAMES {
        text.line(label, 24.0, TextStyle::Label);
    }
    for frame in 0..10 * MONO_CACHE_LINES {
        text.line(&format!("frames {frame} · interval 6.94 ms"), 12.0, TextStyle::Mono);
    }
    assert_eq!(text.mono_cache.len(), MONO_CACHE_LINES);
    for label in NAMES {
        let key = ((*label).to_owned(), 24f32.to_bits());
        assert!(text.label_cache.get(&key).is_some(), "{label} was dropped");
    }
}

#[test]
fn one_line_turns_every_break_and_control_into_a_space() {
    assert!(matches!(one_line("Alice Rossi"), Cow::Borrowed("Alice Rossi")));
    let raw = "a\tb\r\nc\u{b}d\u{c}e\u{85}f\u{2028}g\u{2029}h\u{0}i";
    assert_eq!(one_line(raw), "a b  c d e f g h i");
}

#[test]
fn sizes_that_are_not_positive_numbers_give_nothing_to_draw() {
    let mut text = TextEngine::new([]).expect("an engine over the system fonts");
    for size in [0.0, -12.0, f32::NAN, f32::INFINITY] {
        assert_eq!(text.line("Alice", size, TextStyle::Label), TextLine::default(), "size {size}");
        assert!(text.line_fitting("Alice", size, TextStyle::Label, 100.0).is_none(), "size {size}");
    }
}

#[test]
fn an_unreadable_font_file_is_an_error() {
    let missing = fonts_dir().join("no-such-font.ttf");
    let error = TextEngine::new([FontSource::File(missing.clone())]).expect_err("a missing file");
    assert!(matches!(&error, FontError::Read { path, .. } if *path == missing), "{error}");
}

#[test]
fn data_that_holds_no_font_is_an_error_naming_its_source() {
    let error = TextEngine::new([FontSource::Bytes(b"not a font".to_vec())]).expect_err("no font in the data");
    assert!(matches!(error, FontError::NoFont { index: 0 }), "{error}");
    let Some(files) = font_files() else { return };
    let count = files.len();
    let sources = files.into_iter().map(FontSource::File).chain([FontSource::Embedded(b"")]);
    let error = TextEngine::new(sources).expect_err("no font in the last source");
    assert!(matches!(error, FontError::NoFont { index } if index == count), "{error}");
}
