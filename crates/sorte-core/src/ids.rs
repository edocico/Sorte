//! Identifiers of lists and entries: random UUIDs that stay the same across edits and
//! saves, so outcomes, history and undo can point at an entry whatever happens to its
//! position or text.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies an entry for its whole life, across edits and saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(pub Uuid);

impl EntryId {
    /// A new random id.
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Identifies a list; it also names the list's file in the data dir (`lists/<id>.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListId(pub Uuid);

impl ListId {
    /// A new random id.
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ListId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_differ() {
        assert_ne!(EntryId::random(), EntryId::random());
        assert_ne!(ListId::random(), ListId::random());
    }

    #[test]
    fn ids_display_and_serialize_as_hyphenated_uuids() {
        let uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let text = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(EntryId(uuid).to_string(), text);
        assert_eq!(ListId(uuid).to_string(), text);
        assert_eq!(serde_json::to_string(&EntryId(uuid)).unwrap(), format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<ListId>(&format!("\"{text}\"")).unwrap(), ListId(uuid));
    }
}
