//! The list document: everything that belongs to one list, saved by `sorte-store` as one
//! self-contained JSON file.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DrawRecord, Entry, EntryId, ListId, ListSettings, SPIN_DURATION_SECS, WEIGHT_RANGE};

/// The version of the list format this crate reads and writes. `sorte-store` migrates
/// older documents up to it when it opens them, one step per version.
pub const SCHEMA_VERSION: u32 = 1;

/// The picker a new list starts with: the wheel, first in the registry.
pub const DEFAULT_PICKER: &str = "wheel";

/// A list: its entries, its settings and the history of its draws.
///
/// Fields with an obvious empty value (description, result titles, picker settings,
/// settings, history) read back as empty or default when missing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListDoc {
    /// The format version of the document: [`SCHEMA_VERSION`] once migrated.
    pub schema: u32,
    /// Identifies the list, and names its file.
    pub id: ListId,
    /// The list's name, shown in the top bar and in the list of lists.
    pub title: String,
    /// Free text about the list (v1).
    #[serde(default)]
    pub description: String,
    /// Titles of the result overlay, taken in turn from one draw to the next (v1).
    #[serde(default)]
    pub result_titles: Vec<String>,
    /// The id of the picker that stages this list's draws, such as `"wheel"`.
    pub picker: String,
    /// That picker's own settings (the wheel's speed, mystery mode). Opaque here: the
    /// picker's settings schema validates them.
    #[serde(default = "empty_object")]
    pub picker_settings: Value,
    /// Settings every picker honours: duration and sounds.
    #[serde(default)]
    pub settings: ListSettings,
    /// The entries, in the order the list editor shows them.
    pub entries: Vec<Entry>,
    /// Past draws, oldest first, at most [`HISTORY_LIMIT`](crate::HISTORY_LIMIT).
    #[serde(default)]
    pub history: Vec<DrawRecord>,
    /// When the list was created.
    pub created_at: DateTime<Utc>,
    /// The last change to anything in the document, history included.
    pub updated_at: DateTime<Utc>,
}

impl ListDoc {
    /// An empty list on the default picker, with default settings.
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            schema: SCHEMA_VERSION,
            id: ListId::random(),
            title: title.into(),
            description: String::new(),
            result_titles: Vec::new(),
            picker: DEFAULT_PICKER.to_owned(),
            picker_settings: empty_object(),
            settings: ListSettings::default(),
            entries: Vec::new(),
            history: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// The entry with this id, if the list has it.
    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// The entries the picker shows and the draw chooses among: the enabled ones, in order.
    pub fn visible_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.enabled)
    }

    /// Checks what serde cannot: the format version, the ranges of weights and duration,
    /// and that no two entries share an id. Reports the first problem found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != SCHEMA_VERSION {
            return Err(ValidationError::WrongSchema { found: self.schema, expected: SCHEMA_VERSION });
        }
        let secs = self.settings.spin_duration_secs;
        if !SPIN_DURATION_SECS.contains(&secs) {
            return Err(ValidationError::SpinDurationOutOfRange { secs });
        }
        if let Some(entry) = self.entries.iter().find(|entry| !WEIGHT_RANGE.contains(&entry.weight)) {
            return Err(ValidationError::WeightOutOfRange { entry: entry.id, weight: entry.weight });
        }
        match self.shared_entry_id() {
            Some(id) => Err(ValidationError::DuplicateEntryId(id)),
            None => Ok(()),
        }
    }

    /// An id that two entries share, the smallest if several are. Ids must be unique: the
    /// draw, the result actions and undo find entries by id, hidden ones included, and
    /// would otherwise take one entry for another.
    pub(crate) fn shared_entry_id(&self) -> Option<EntryId> {
        // Sorted rather than hashed: every draw checks this, and in debug builds hashing made
        // the fairness test's million draws four times slower, sorting only twice.
        let mut ids: Vec<EntryId> = self.entries.iter().map(|entry| entry.id).collect();
        ids.sort_unstable();
        ids.windows(2).find(|pair| pair[0] == pair[1]).map(|pair| pair[0])
    }
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// A problem [`ListDoc::validate`] found.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The document is in another format version: migrate it first.
    #[error("the list is in format version {found}, not {expected}")]
    WrongSchema {
        /// The document's version.
        found: u32,
        /// [`SCHEMA_VERSION`].
        expected: u32,
    },
    /// An entry's weight is outside [`WEIGHT_RANGE`].
    #[error("entry {entry} has weight {weight}, outside {}..={}", WEIGHT_RANGE.start(), WEIGHT_RANGE.end())]
    WeightOutOfRange {
        /// The entry.
        entry: EntryId,
        /// Its weight.
        weight: u32,
    },
    /// The spin duration is outside [`SPIN_DURATION_SECS`].
    #[error("the spin lasts {secs} s, outside {}..={} s", SPIN_DURATION_SECS.start(), SPIN_DURATION_SECS.end())]
    SpinDurationOutOfRange {
        /// The duration, in seconds.
        secs: u32,
    },
    /// Two entries share this id.
    #[error("two entries have id {0}")]
    DuplicateEntryId(EntryId),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::testing;

    fn list_of(texts: &[&str]) -> ListDoc {
        let mut list = ListDoc::new("Pranzo");
        list.entries = texts.iter().copied().map(Entry::new).collect();
        list
    }

    #[test]
    fn a_new_list_is_valid_empty_and_on_the_wheel() {
        let list = ListDoc::new("Pranzo");
        assert_eq!(list.validate(), Ok(()));
        assert_eq!((list.schema, list.title.as_str(), list.picker.as_str()), (SCHEMA_VERSION, "Pranzo", "wheel"));
        assert_eq!(list.picker_settings, json!({}));
        assert_eq!(list.settings, ListSettings::default());
        assert!(list.description.is_empty() && list.result_titles.is_empty());
        assert!(list.entries.is_empty() && list.history.is_empty());
        assert_eq!(list.created_at, list.updated_at);
        assert_ne!(ListDoc::new("Pranzo").id, list.id);
    }

    #[test]
    fn finds_entries_by_id_and_shows_only_enabled_ones() {
        let mut list = list_of(&["a", "b", "c"]);
        list.entries[1].enabled = false;
        let b = list.entries[1].id;
        assert_eq!(list.entry(b).map(|entry| entry.text.as_str()), Some("b"));
        assert_eq!(list.entry(EntryId::random()), None);
        let visible: Vec<&str> = list.visible_entries().map(|entry| entry.text.as_str()).collect();
        assert_eq!(visible, ["a", "c"]);
    }

    #[test]
    fn validation_reports_a_wrong_schema() {
        let mut list = list_of(&["a"]);
        list.schema = 2;
        assert_eq!(list.validate(), Err(ValidationError::WrongSchema { found: 2, expected: 1 }));
    }

    #[test]
    fn validation_reports_weights_outside_the_range() {
        let mut list = list_of(&["a", "b"]);
        list.entries[0].weight = 1;
        list.entries[1].weight = 1000;
        assert_eq!(list.validate(), Ok(()));
        for weight in [0, 1001, u32::MAX] {
            list.entries[1].weight = weight;
            let entry = list.entries[1].id;
            assert_eq!(list.validate(), Err(ValidationError::WeightOutOfRange { entry, weight }));
        }
    }

    #[test]
    fn validation_reports_durations_outside_the_range() {
        let mut list = list_of(&["a"]);
        for secs in [1, 30] {
            list.settings.spin_duration_secs = secs;
            assert_eq!(list.validate(), Ok(()));
        }
        for secs in [0, 31] {
            list.settings.spin_duration_secs = secs;
            assert_eq!(list.validate(), Err(ValidationError::SpinDurationOutOfRange { secs }));
        }
    }

    #[test]
    fn validation_reports_shared_ids() {
        let mut list = list_of(&["a", "b", "c"]);
        list.entries[2].id = list.entries[0].id;
        assert_eq!(list.validate(), Err(ValidationError::DuplicateEntryId(list.entries[0].id)));
    }

    #[test]
    fn missing_optional_fields_read_back_as_defaults() {
        let list = ListDoc::new("Pranzo");
        let json = json!({
            "schema": 1,
            "id": list.id,
            "title": "Pranzo",
            "picker": "wheel",
            "entries": [],
            "created_at": list.created_at,
            "updated_at": list.updated_at,
        });
        assert_eq!(serde_json::from_value::<ListDoc>(json).unwrap(), list);
    }

    proptest! {
        #[test]
        fn documents_survive_a_json_round_trip(list in testing::list()) {
            let text = serde_json::to_string(&list).unwrap();
            prop_assert_eq!(serde_json::from_str::<ListDoc>(&text).unwrap(), list);
        }
    }
}
