//! Decides the outcome of a draw, and owns the list model: lists, entries, result
//! actions and history (design doc, § Algoritmi chiave and § Modello dati e salvataggio).
//! Depends on no other workspace crate, so it is tested without a GPU or a UI.
//!
//! - The model: [`ListDoc`] and its [`Entry`]s, [`ListSettings`] and [`DrawRecord`]s, in
//!   the file format [`SCHEMA_VERSION`] (`sorte-store` reads, migrates and writes it).
//! - The fair draw: [`draw`] and [`draw_ranking`] decide an [`Outcome`] from the weights
//!   alone; pickers only stage it.
//! - After a draw: [`apply_result`] records it and applies a [`ResultAction`].
//! - Undo and redo: every change is an [`Edit`] whose application returns its inverse.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod asset;
mod color;
mod draw;
mod edit;
mod entry;
mod history;
mod ids;
mod list;
mod settings;
#[cfg(test)]
mod testing;
mod text;

pub use asset::{AssetRef, AssetRefError};
pub use color::{ParseColorError, Rgba8};
pub use draw::{DrawError, Outcome, RankingRule, draw, draw_index, draw_ranking, os_rng};
pub use edit::{Edit, EditError};
pub use entry::{DEFAULT_WEIGHT, Entry, WEIGHT_RANGE};
pub use history::{DrawRecord, HISTORY_LIMIT, ResultAction, apply_result};
pub use ids::{EntryId, ListId};
pub use list::{DEFAULT_PICKER, ListDoc, SCHEMA_VERSION, ValidationError};
pub use settings::{DEFAULT_SPIN_DURATION_SECS, ListSettings, SPIN_DURATION_SECS, SoundChoice, Sounds};
pub use text::{entries_from_lines, normalize_text};
