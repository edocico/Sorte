//! One entry of a list: what the pickers show and the draw chooses among.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::{AssetRef, EntryId, Rgba8};

/// The weights an entry may have. Integer weights keep the draw's arithmetic exact.
pub const WEIGHT_RANGE: RangeInclusive<u32> = 1..=1000;

/// The weight of a new entry: when nobody sets weights, every entry is equally likely.
pub const DEFAULT_WEIGHT: u32 = 1;

/// An entry of a list. Missing `weight`, `enabled` and `count` read back as a new entry's
/// values, so a hand-written entry needs only its id and text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Stays the same across edits, so outcomes, history and undo refer to it.
    pub id: EntryId,
    /// What the entry is; the list editor shows it, and the picker too unless a label replaces it.
    pub text: String,
    /// Replaces the text on the picker, for example on the wheel's slice (v1).
    pub label: Option<String>,
    /// An image shown with the text or instead of it (v1).
    pub image: Option<AssetRef>,
    /// The entry's chance relative to the others, within [`WEIGHT_RANGE`]; the wheel's slice
    /// is proportional to it.
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Overrides the theme's colour for this entry (v1).
    pub color: Option<Rgba8>,
    /// False hides the entry: it stays in the list but the picker and the draw skip it.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// The entry's score: how many times the "add count" result action was applied to it.
    #[serde(default)]
    pub count: u32,
}

impl Entry {
    /// A visible entry with a fresh id, the default weight and no count.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: EntryId::random(),
            text: text.into(),
            label: None,
            image: None,
            weight: DEFAULT_WEIGHT,
            color: None,
            enabled: true,
            count: 0,
        }
    }
}

fn default_weight() -> u32 {
    DEFAULT_WEIGHT
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn a_new_entry_is_visible_with_weight_one() {
        let entry = Entry::new("Pizza");
        assert_eq!(entry.text, "Pizza");
        assert_eq!((entry.weight, entry.enabled, entry.count), (1, true, 0));
        assert_eq!((entry.label, entry.image, entry.color), (None, None, None));
        assert!(WEIGHT_RANGE.contains(&DEFAULT_WEIGHT));
        assert_ne!(Entry::new("Pizza").id, Entry::new("Pizza").id);
    }

    #[test]
    fn an_entry_with_only_id_and_text_reads_as_a_new_one() {
        let id = EntryId(Uuid::from_u128(7));
        let entry: Entry = serde_json::from_value(json!({ "id": id, "text": "Sushi" })).unwrap();
        assert_eq!(entry, Entry { id, ..Entry::new("Sushi") });
    }
}
