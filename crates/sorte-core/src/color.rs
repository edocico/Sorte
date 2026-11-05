//! Entry colours. Files store them as `"#rrggbbaa"` strings, so a list stays readable and
//! editable by hand.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// An sRGB colour with straight (not premultiplied) alpha, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba8 {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Opacity: 0 is transparent, 255 opaque.
    pub a: u8,
}

/// Formats as `#rrggbbaa`, lowercase: the form files store.
impl fmt::Display for Rgba8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { r, g, b, a } = self;
        write!(f, "#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// Parses `#rrggbbaa`, or `#rrggbb` for an opaque colour, in either case.
impl FromStr for Rgba8 {
    type Err = ParseColorError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseColorError { input: text.to_owned() };
        let digits = text.strip_prefix('#').ok_or_else(invalid)?;
        // `to_digit` refuses the sign that `u8::from_str_radix` would accept ("+f").
        let nibbles: Vec<u8> =
            digits.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>().ok_or_else(invalid)?;
        let (pairs, odd_digit) = nibbles.as_chunks::<2>();
        let channels: Vec<u8> = pairs.iter().map(|&[high, low]| (high << 4) | low).collect();
        match (channels.as_slice(), odd_digit) {
            (&[r, g, b], []) => Ok(Self { r, g, b, a: u8::MAX }),
            (&[r, g, b, a], []) => Ok(Self { r, g, b, a }),
            _ => Err(invalid()),
        }
    }
}

impl Serialize for Rgba8 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgba8 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(de::Error::custom)
    }
}

/// A string that is not a colour in the `#rrggbb` or `#rrggbbaa` form.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{input:?} is not a colour: write it as #rrggbb or #rrggbbaa")]
pub struct ParseColorError {
    input: String,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn displays_as_lowercase_rrggbbaa() {
        assert_eq!(Rgba8 { r: 0x01, g: 0x23, b: 0xab, a: 0xff }.to_string(), "#0123abff");
        assert_eq!(Rgba8 { r: 0, g: 0, b: 0, a: 0 }.to_string(), "#00000000");
    }

    #[test]
    fn parses_rrggbbaa_and_opaque_rrggbb_in_either_case() {
        assert_eq!("#0123abcd".parse(), Ok(Rgba8 { r: 0x01, g: 0x23, b: 0xab, a: 0xcd }));
        assert_eq!("#E4572E80".parse(), Ok(Rgba8 { r: 0xe4, g: 0x57, b: 0x2e, a: 0x80 }));
        assert_eq!("#e4572e".parse(), Ok(Rgba8 { r: 0xe4, g: 0x57, b: 0x2e, a: 0xff }));
    }

    #[test]
    fn rejects_anything_else() {
        for text in ["", "#", "e4572eff", "#e4572", "#e4572ef", "#e4572eff0", "#g4572eff", "#+f+f+f", "#éé", " #e4572e"]
        {
            assert!(text.parse::<Rgba8>().is_err(), "{text:?} parsed");
        }
    }

    #[test]
    fn serializes_as_a_string_and_refuses_bad_ones() {
        let color = Rgba8 { r: 0x17, g: 0xbe, b: 0xbb, a: 0x80 };
        assert_eq!(serde_json::to_string(&color).unwrap(), "\"#17bebb80\"");
        assert_eq!(serde_json::from_str::<Rgba8>("\"#17bebb80\"").unwrap(), color);
        assert!(serde_json::from_str::<Rgba8>("\"#17bebb8\"").is_err());
        assert!(serde_json::from_str::<Rgba8>("[23, 190, 187, 128]").is_err());
    }

    proptest! {
        #[test]
        fn display_parses_back(rgba in any::<[u8; 4]>()) {
            let [r, g, b, a] = rgba;
            let color = Rgba8 { r, g, b, a };
            prop_assert_eq!(color.to_string().parse(), Ok(color));
        }
    }
}
