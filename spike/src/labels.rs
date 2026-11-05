//! Sample lists, the slice palette and label contrast.

use vello::peniko::Color;

/// Which sample entries fill the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelSet {
    Names,
    /// Complex scripts, right-to-left text and colour emoji.
    Scripts,
    Numbered,
}

impl LabelSet {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Scripts,
            2 => Self::Numbered,
            _ => Self::Names,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "names" => Some(Self::Names),
            "scripts" => Some(Self::Scripts),
            "numbered" => Some(Self::Numbered),
            _ => None,
        }
    }
}

const NAMES: &[&str] = &[
    "Alice", "Bruno", "Carla", "Dario", "Elena", "Fabio", "Giulia", "Hugo", "Irene", "Luca",
    "Marta", "Nicola", "Olga", "Paolo", "Rita", "Sara", "Tommaso", "Ugo", "Vera", "Zeno",
    "Beatrice", "Massimiliano", "Ludovica", "Pierfrancesco",
];

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

/// `n` entries from the chosen set, cycling through it and numbering repeats.
pub fn entries(set: LabelSet, n: usize) -> Vec<String> {
    let source = match set {
        LabelSet::Names => NAMES,
        LabelSet::Scripts => SCRIPTS,
        LabelSet::Numbered => return (1..=n).map(|i| format!("Voce {i}")).collect(),
    };
    (0..n)
        .map(|i| {
            let base = source[i % source.len()];
            match i / source.len() {
                0 => base.to_owned(),
                round => format!("{base} {}", round + 1),
            }
        })
        .collect()
}

const PALETTE: [u32; 12] = [
    0xef476f, 0xf78c6b, 0xffd166, 0x06d6a0, 0x118ab2, 0x073b4c, 0x8338ec, 0xff006e, 0x3a86ff,
    0xfb5607, 0x2ec4b6, 0x9b5de5,
];

fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Colour of slice `i` out of `n`, cycling the palette without giving the first and
/// last slices the same colour.
pub fn slice_color(i: usize, n: usize) -> Color {
    let len = PALETTE.len();
    let mut k = i % len;
    if n > 1 && i == n - 1 && k == 0 {
        k = 1;
    }
    rgb(PALETTE[k])
}

/// Black or white, whichever contrasts more with `background`. One of them always
/// reaches at least √21 ≈ 4.58:1, above the WCAG 4.5:1 threshold.
pub fn label_color(background: Color) -> Color {
    let l = relative_luminance(background);
    let with_white = 1.05 / (l + 0.05);
    let with_black = (l + 0.05) / 0.05;
    if with_white >= with_black { Color::WHITE } else { Color::BLACK }
}

fn relative_luminance(c: Color) -> f64 {
    let [r, g, b, _] = c.components;
    let lin = |v: f32| {
        let v = f64::from(v);
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn first_and_last_slices_differ_and_neighbours_differ() {
        for n in 2..50 {
            let colors: Vec<_> = (0..n).map(|i| slice_color(i, n)).collect();
            assert_ne!(colors[0], colors[n - 1], "n = {n}");
            assert!(colors.windows(2).all(|w| w[0] != w[1]), "n = {n}");
        }
    }

    #[test]
    fn labels_meet_wcag_aa() {
        for hex in PALETTE {
            let bg = rgb(hex);
            assert!(contrast(bg, label_color(bg)) >= 4.5, "{hex:06x}");
        }
    }
}
