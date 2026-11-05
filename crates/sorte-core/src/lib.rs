//! Decides the outcome of a draw, and owns the list model: lists, entries, result
//! actions and history (design doc, § Algoritmi chiave and § Modello dati e salvataggio).
//! Depends on no other workspace crate, so it is tested without a GPU or a UI.
#![forbid(unsafe_code)]

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

/// What a draw decided. Pickers only stage it: they never choose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The entry chosen in proportion to its weight.
    pub selected: EntryId,
    /// The full arrival order, for ranking pickers only (M2).
    pub ranking: Option<Vec<EntryId>>,
}
