//! A list's own settings for staging its draws, whatever the picker. Picker-specific
//! settings (the wheel's speed, mystery mode) live in `ListDoc::picker_settings` instead.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::AssetRef;

/// The spin durations a list may ask for, in whole seconds (PickerWheel's 1–30 s).
pub const SPIN_DURATION_SECS: RangeInclusive<u32> = 1..=30;

/// The spin duration of a new list, in seconds.
pub const DEFAULT_SPIN_DURATION_SECS: u32 = 8;

/// Settings every picker honours. Missing fields read back as their defaults, so a file
/// written before a setting existed still opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListSettings {
    /// How long a draw is staged, in seconds, within [`SPIN_DURATION_SECS`]. A fling of the
    /// wheel may stage one draw for a different time.
    pub spin_duration_secs: u32,
    /// What plays during a draw.
    pub sounds: Sounds,
}

impl Default for ListSettings {
    fn default() -> Self {
        Self { spin_duration_secs: DEFAULT_SPIN_DURATION_SECS, sounds: Sounds::default() }
    }
}

/// The sounds of a draw. Missing fields read back as [`SoundChoice::Default`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sounds {
    /// When the draw starts.
    pub start: SoundChoice,
    /// Each time the pointer passes a slice edge (or the picker's equivalent).
    pub ticks: SoundChoice,
    /// When the picker lands on the outcome.
    pub end: SoundChoice,
}

/// Which sound plays at one moment of a draw.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundChoice {
    /// The sound that ships with the app.
    #[default]
    Default,
    /// Silence.
    Off,
    /// A sound the user added, kept in the asset store (v1).
    Custom(AssetRef),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_eight_seconds_and_the_bundled_sounds() {
        let settings = ListSettings::default();
        assert_eq!(settings.spin_duration_secs, 8);
        assert!(SPIN_DURATION_SECS.contains(&settings.spin_duration_secs));
        let Sounds { start, ticks, end } = settings.sounds;
        assert_eq!([start, ticks, end], [SoundChoice::Default, SoundChoice::Default, SoundChoice::Default]);
    }

    #[test]
    fn missing_fields_read_back_as_defaults() {
        assert_eq!(serde_json::from_value::<ListSettings>(json!({})).unwrap(), ListSettings::default());
        let settings: ListSettings = serde_json::from_value(json!({ "sounds": { "ticks": "off" } })).unwrap();
        assert_eq!(settings.spin_duration_secs, DEFAULT_SPIN_DURATION_SECS);
        assert_eq!(settings.sounds, Sounds { ticks: SoundChoice::Off, ..Sounds::default() });
    }

    #[test]
    fn sound_choices_serialize_in_snake_case() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let sounds = Sounds {
            start: SoundChoice::Default,
            ticks: SoundChoice::Off,
            end: SoundChoice::Custom(AssetRef::new(hash, "ogg").unwrap()),
        };
        let json = json!({ "start": "default", "ticks": "off", "end": { "custom": { "sha256": hash, "ext": "ogg" } } });
        assert_eq!(serde_json::to_value(&sounds).unwrap(), json);
        assert_eq!(serde_json::from_value::<Sounds>(json).unwrap(), sounds);
    }
}
