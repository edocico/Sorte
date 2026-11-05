//! Reversible edits of a list, for undo and redo (F3). Every change to entries and
//! settings is an [`Edit`]: applying one returns its inverse, which the undo stack in
//! `sorte-app` keeps; applying the inverse returns the original edit, for redo.

use std::collections::HashSet;
use std::mem;
use std::ops::Range;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{Entry, EntryId, ListDoc, ListSettings, SPIN_DURATION_SECS, WEIGHT_RANGE};

/// One reversible change to a list. Positions are indexes into the list's entries.
///
/// Edits keep a valid list valid (see [`ListDoc::validate`]): one that would give two
/// entries the same id, an entry a weight outside [`WEIGHT_RANGE`], or the spin a duration
/// outside [`SPIN_DURATION_SECS`] is refused. Outcomes, history and [`Edit::ReplaceEntry`]
/// find entries by id, and the draw refuses weights out of range.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Inserts entries so that the first one lands at `at`; `at` equal to the number of
    /// entries appends them.
    InsertEntries {
        /// Where the first inserted entry lands.
        at: usize,
        /// The entries to insert, in order, with ids new to the list.
        entries: Vec<Entry>,
    },
    /// Removes the entries at these positions.
    RemoveEntries(Range<usize>),
    /// Replaces the entry that has the same id: how one entry's text, weight, visibility
    /// or anything else changes.
    ReplaceEntry(Entry),
    /// Moves an entry so that it ends up at position `to`.
    MoveEntry {
        /// The entry's position before the move.
        from: usize,
        /// Its position after the move.
        to: usize,
    },
    /// Replaces all the entries at once, for bulk operations such as sorting, shuffling or
    /// removing empty entries.
    ReplaceAllEntries(Vec<Entry>),
    /// Renames the list.
    SetTitle(String),
    /// Sets the list's description.
    SetDescription(String),
    /// Sets the titles of the result overlay.
    SetResultTitles(Vec<String>),
    /// Sets the settings every picker honours.
    SetSettings(ListSettings),
    /// Switches the list to another picker, together with that picker's settings.
    SetPicker {
        /// The picker's id.
        picker: String,
        /// Its settings.
        settings: Value,
    },
    /// Sets the current picker's settings.
    SetPickerSettings(Value),
}

impl Edit {
    /// Applies the edit and returns its inverse: the edit that, applied next, puts the list
    /// back as it was, except `updated_at`, which every applied edit sets to now. A refused
    /// edit leaves the list untouched.
    ///
    /// On a list that passes [`ListDoc::validate`], the inverse always applies, and so does
    /// the inverse it returns in turn: undo and redo cannot fail. On a list that does not,
    /// an inverse that would bring back an invalid value is refused like any other edit.
    pub fn apply(self, list: &mut ListDoc) -> Result<Edit, EditError> {
        self.apply_at(list, Utc::now())
    }

    /// [`Edit::apply`], recording `now` as the time of the change.
    pub(crate) fn apply_at(self, list: &mut ListDoc, now: DateTime<Utc>) -> Result<Edit, EditError> {
        let inverse = match self {
            Edit::InsertEntries { at, entries } => {
                let len = list.entries.len();
                if at > len {
                    return Err(EditError::IndexOutOfRange { index: at, len });
                }
                check_new_entries(&list.entries, &entries)?;
                let end = at + entries.len();
                list.entries.splice(at..at, entries);
                Edit::RemoveEntries(at..end)
            }
            Edit::RemoveEntries(range) => {
                let len = list.entries.len();
                if range.start > range.end || range.end > len {
                    return Err(EditError::InvalidRange { range, len });
                }
                let at = range.start;
                Edit::InsertEntries { at, entries: list.entries.drain(range).collect() }
            }
            Edit::ReplaceEntry(entry) => {
                let slot =
                    list.entries.iter_mut().find(|old| old.id == entry.id).ok_or(EditError::UnknownEntry(entry.id))?;
                check_weight(&entry)?;
                Edit::ReplaceEntry(mem::replace(slot, entry))
            }
            Edit::MoveEntry { from, to } => {
                let len = list.entries.len();
                if let Some(index) = [from, to].into_iter().find(|&index| index >= len) {
                    return Err(EditError::IndexOutOfRange { index, len });
                }
                let entry = list.entries.remove(from);
                list.entries.insert(to, entry);
                Edit::MoveEntry { from: to, to: from }
            }
            Edit::ReplaceAllEntries(entries) => {
                check_new_entries(&[], &entries)?;
                Edit::ReplaceAllEntries(mem::replace(&mut list.entries, entries))
            }
            Edit::SetTitle(title) => Edit::SetTitle(mem::replace(&mut list.title, title)),
            Edit::SetDescription(description) => Edit::SetDescription(mem::replace(&mut list.description, description)),
            Edit::SetResultTitles(titles) => Edit::SetResultTitles(mem::replace(&mut list.result_titles, titles)),
            Edit::SetSettings(settings) => {
                let secs = settings.spin_duration_secs;
                if !SPIN_DURATION_SECS.contains(&secs) {
                    return Err(EditError::SpinDurationOutOfRange { secs });
                }
                Edit::SetSettings(mem::replace(&mut list.settings, settings))
            }
            Edit::SetPicker { picker, settings } => Edit::SetPicker {
                picker: mem::replace(&mut list.picker, picker),
                settings: mem::replace(&mut list.picker_settings, settings),
            },
            Edit::SetPickerSettings(settings) => {
                Edit::SetPickerSettings(mem::replace(&mut list.picker_settings, settings))
            }
        };
        list.updated_at = now;
        Ok(inverse)
    }
}

/// Refuses `added` if one of its entries has a weight outside [`WEIGHT_RANGE`], or an id
/// that is in `kept` or earlier in `added`.
fn check_new_entries(kept: &[Entry], added: &[Entry]) -> Result<(), EditError> {
    let mut ids: HashSet<EntryId> = kept.iter().map(|entry| entry.id).collect();
    for entry in added {
        check_weight(entry)?;
        if !ids.insert(entry.id) {
            return Err(EditError::DuplicateEntryId(entry.id));
        }
    }
    Ok(())
}

/// Refuses an entry whose weight is outside [`WEIGHT_RANGE`].
fn check_weight(entry: &Entry) -> Result<(), EditError> {
    if WEIGHT_RANGE.contains(&entry.weight) {
        Ok(())
    } else {
        Err(EditError::WeightOutOfRange { entry: entry.id, weight: entry.weight })
    }
}

/// Why an edit was refused. The list is left as it was.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// A position past the end of the list.
    #[error("position {index} is outside a list of {len} entries")]
    IndexOutOfRange {
        /// The position.
        index: usize,
        /// The number of entries.
        len: usize,
    },
    /// A range of positions that is reversed or runs past the end of the list.
    #[error("positions {range:?} are not a range within a list of {len} entries")]
    InvalidRange {
        /// The range.
        range: Range<usize>,
        /// The number of entries.
        len: usize,
    },
    /// No entry has this id.
    #[error("no entry has id {0}")]
    UnknownEntry(EntryId),
    /// The edit would give two entries this id, or, for [`apply_result`](crate::apply_result),
    /// two entries already have it.
    #[error("two entries cannot share id {0}")]
    DuplicateEntryId(EntryId),
    /// The edit would give an entry a weight outside [`WEIGHT_RANGE`].
    #[error("entry {entry} cannot have weight {weight}, outside {}..={}", WEIGHT_RANGE.start(), WEIGHT_RANGE.end())]
    WeightOutOfRange {
        /// The entry.
        entry: EntryId,
        /// The weight.
        weight: u32,
    },
    /// The edit would make the spin last a time outside [`SPIN_DURATION_SECS`].
    #[error("the spin cannot last {secs} s, outside {}..={} s", SPIN_DURATION_SECS.start(), SPIN_DURATION_SECS.end())]
    SpinDurationOutOfRange {
        /// The duration, in seconds.
        secs: u32,
    },
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::TestCaseError;
    use serde_json::json;

    use super::*;
    use crate::{SoundChoice, testing};

    fn list_of(texts: &[&str]) -> ListDoc {
        let mut list = ListDoc::new("Pranzo");
        list.entries = texts.iter().copied().map(Entry::new).collect();
        list
    }

    fn texts(list: &ListDoc) -> Vec<&str> {
        list.entries.iter().map(|entry| entry.text.as_str()).collect()
    }

    /// The list without `updated_at`, which every applied edit sets to now.
    fn content(list: &ListDoc) -> ListDoc {
        ListDoc { updated_at: list.created_at, ..list.clone() }
    }

    /// Applies `edit`, checks that its inverse restores the list, and returns the edited list.
    fn apply_and_undo(list: &ListDoc, edit: Edit) -> ListDoc {
        let mut edited = list.clone();
        let inverse = edit.clone().apply(&mut edited).unwrap();
        let mut undone = edited.clone();
        let redo = inverse.apply(&mut undone).unwrap();
        assert_eq!(content(&undone), content(list));
        assert_eq!(redo, edit);
        edited
    }

    /// Checks that `edit` is refused with `error` and leaves the list untouched.
    fn assert_refused(list: &ListDoc, edit: Edit, error: EditError) {
        let mut edited = list.clone();
        assert_eq!(edit.apply(&mut edited), Err(error));
        assert_eq!(&edited, list);
    }

    #[test]
    fn inserts_at_any_position_up_to_the_end() {
        let list = list_of(&["a", "b", "c"]);
        let new = vec![Entry::new("x"), Entry::new("y")];
        let edited = apply_and_undo(&list, Edit::InsertEntries { at: 1, entries: new.clone() });
        assert_eq!(texts(&edited), ["a", "x", "y", "b", "c"]);
        let edited = apply_and_undo(&list, Edit::InsertEntries { at: 3, entries: new.clone() });
        assert_eq!(texts(&edited), ["a", "b", "c", "x", "y"]);
        let mut inserted = list.clone();
        let inverse = Edit::InsertEntries { at: 1, entries: new.clone() }.apply(&mut inserted).unwrap();
        assert_eq!(inverse, Edit::RemoveEntries(1..3));
        assert_refused(
            &list,
            Edit::InsertEntries { at: 4, entries: new },
            EditError::IndexOutOfRange { index: 4, len: 3 },
        );
    }

    #[test]
    fn insertion_refuses_ids_already_in_the_list_or_repeated() {
        let list = list_of(&["a", "b"]);
        let copy = list.entries[1].clone();
        assert_refused(
            &list,
            Edit::InsertEntries { at: 0, entries: vec![copy.clone()] },
            EditError::DuplicateEntryId(copy.id),
        );
        let twice = Entry::new("x");
        let entries = vec![twice.clone(), Entry::new("y"), twice.clone()];
        assert_refused(&list, Edit::InsertEntries { at: 2, entries }, EditError::DuplicateEntryId(twice.id));
    }

    #[test]
    fn removes_a_range_of_positions() {
        let list = list_of(&["a", "b", "c", "d"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::RemoveEntries(1..3))), ["a", "d"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::RemoveEntries(0..4))), Vec::<&str>::new());
        assert_eq!(texts(&apply_and_undo(&list, Edit::RemoveEntries(2..2))), ["a", "b", "c", "d"]);
        let mut removed = list.clone();
        let inverse = Edit::RemoveEntries(1..3).apply(&mut removed).unwrap();
        assert_eq!(inverse, Edit::InsertEntries { at: 1, entries: list.entries[1..3].to_vec() });
        assert_refused(&list, Edit::RemoveEntries(2..5), EditError::InvalidRange { range: 2..5, len: 4 });
        let reversed = Range { start: 3, end: 1 };
        assert_refused(
            &list,
            Edit::RemoveEntries(reversed.clone()),
            EditError::InvalidRange { range: reversed, len: 4 },
        );
    }

    #[test]
    fn replaces_the_entry_with_the_same_id() {
        let list = list_of(&["a", "b", "c"]);
        let changed = Entry { text: "B".into(), weight: 7, enabled: false, ..list.entries[1].clone() };
        let edited = apply_and_undo(&list, Edit::ReplaceEntry(changed.clone()));
        assert_eq!(edited.entries[1], changed);
        assert_eq!(texts(&edited), ["a", "B", "c"]);
        let stranger = Entry::new("z");
        assert_refused(&list, Edit::ReplaceEntry(stranger.clone()), EditError::UnknownEntry(stranger.id));
    }

    #[test]
    fn moves_an_entry_to_its_new_position() {
        let list = list_of(&["a", "b", "c", "d"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::MoveEntry { from: 0, to: 2 })), ["b", "c", "a", "d"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::MoveEntry { from: 3, to: 0 })), ["d", "a", "b", "c"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::MoveEntry { from: 1, to: 1 })), ["a", "b", "c", "d"]);
        assert_refused(&list, Edit::MoveEntry { from: 4, to: 0 }, EditError::IndexOutOfRange { index: 4, len: 4 });
        assert_refused(&list, Edit::MoveEntry { from: 0, to: 4 }, EditError::IndexOutOfRange { index: 4, len: 4 });
    }

    #[test]
    fn replaces_all_entries_with_unique_ids() {
        let list = list_of(&["a", "b", "c"]);
        let reversed: Vec<Entry> = list.entries.iter().rev().cloned().collect();
        assert_eq!(texts(&apply_and_undo(&list, Edit::ReplaceAllEntries(reversed))), ["c", "b", "a"]);
        assert_eq!(texts(&apply_and_undo(&list, Edit::ReplaceAllEntries(Vec::new()))), Vec::<&str>::new());
        let repeated = vec![list.entries[0].clone(), list.entries[0].clone()];
        assert_refused(&list, Edit::ReplaceAllEntries(repeated), EditError::DuplicateEntryId(list.entries[0].id));
    }

    #[test]
    fn weights_and_durations_outside_their_ranges_are_refused() {
        let list = list_of(&["a", "b"]);
        let (lightest, heaviest) = (*WEIGHT_RANGE.start(), *WEIGHT_RANGE.end());
        for weight in [lightest, heaviest] {
            let edited = apply_and_undo(&list, Edit::ReplaceEntry(Entry { weight, ..list.entries[0].clone() }));
            assert_eq!(edited.entries[0].weight, weight);
        }
        for weight in [0, heaviest + 1, u32::MAX] {
            // Hidden entries too: the list would fail validate(), and showing the entry
            // again, which changes `enabled` alone, would bring the weight into the draw.
            let changed = Entry { weight, enabled: false, ..list.entries[1].clone() };
            let error = EditError::WeightOutOfRange { entry: changed.id, weight };
            assert_refused(&list, Edit::ReplaceEntry(changed), error);
            let new = Entry { weight, ..Entry::new("x") };
            let error = EditError::WeightOutOfRange { entry: new.id, weight };
            let entries = vec![Entry::new("y"), new.clone()];
            assert_refused(&list, Edit::InsertEntries { at: 1, entries }, error.clone());
            let all = vec![list.entries[0].clone(), new, list.entries[1].clone()];
            assert_refused(&list, Edit::ReplaceAllEntries(all), error);
        }
        let (shortest, longest) = (*SPIN_DURATION_SECS.start(), *SPIN_DURATION_SECS.end());
        for secs in [shortest, longest] {
            let settings = ListSettings { spin_duration_secs: secs, ..ListSettings::default() };
            assert_eq!(apply_and_undo(&list, Edit::SetSettings(settings.clone())).settings, settings);
        }
        for secs in [0, longest + 1] {
            let settings = ListSettings { spin_duration_secs: secs, ..ListSettings::default() };
            assert_refused(&list, Edit::SetSettings(settings), EditError::SpinDurationOutOfRange { secs });
        }
    }

    #[test]
    fn list_fields_swap_with_their_previous_value() {
        let list = list_of(&["a"]);
        assert_eq!(apply_and_undo(&list, Edit::SetTitle("Cena".into())).title, "Cena");
        assert_eq!(apply_and_undo(&list, Edit::SetDescription("Chi cucina".into())).description, "Chi cucina");
        let titles = vec!["Tocca a…".to_owned(), "Oggi:".to_owned()];
        assert_eq!(apply_and_undo(&list, Edit::SetResultTitles(titles.clone())).result_titles, titles);
        let mut settings = ListSettings { spin_duration_secs: 20, ..ListSettings::default() };
        settings.sounds.end = SoundChoice::Off;
        assert_eq!(apply_and_undo(&list, Edit::SetSettings(settings.clone())).settings, settings);
        let edited =
            apply_and_undo(&list, Edit::SetPicker { picker: "horse-race".into(), settings: json!({ "lanes": 6 }) });
        assert_eq!((edited.picker.as_str(), &edited.picker_settings), ("horse-race", &json!({ "lanes": 6 })));
        let edited = apply_and_undo(&list, Edit::SetPickerSettings(json!({ "speed": 9 })));
        assert_eq!((edited.picker.as_str(), &edited.picker_settings), ("wheel", &json!({ "speed": 9 })));
    }

    #[test]
    fn an_applied_edit_records_when_the_list_changed() {
        let mut list = list_of(&["a"]);
        let at = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap();
        Edit::SetTitle("Cena".into()).apply_at(&mut list, at).unwrap();
        assert_eq!(list.updated_at, at);
        let before = Utc::now();
        Edit::SetTitle("Pranzo".into()).apply(&mut list).unwrap();
        assert!(list.updated_at >= before);
    }

    /// An edit to generate, with raw positions that `edit_for` fits to the list at hand.
    #[derive(Clone, Debug)]
    enum Recipe {
        Insert { at: usize, entries: Vec<Entry> },
        InsertCopy { pick: usize, at: usize },
        Remove { start: usize, end: usize },
        Replace { pick: usize, with: Entry },
        ReplaceUnknown(Entry),
        Move { from: usize, to: usize },
        Reverse,
        ReplaceAll(Vec<Entry>),
        Title(String),
        Description(String),
        ResultTitles(Vec<String>),
        Settings(ListSettings),
        Picker(String, Value),
        PickerSettings(Value),
    }

    fn recipe() -> impl Strategy<Value = Recipe> {
        let raw = || 0usize..64;
        prop_oneof![
            (raw(), new_entries(3)).prop_map(|(at, entries)| Recipe::Insert { at, entries }),
            (raw(), raw()).prop_map(|(pick, at)| Recipe::InsertCopy { pick, at }),
            (raw(), raw()).prop_map(|(start, end)| Recipe::Remove { start, end }),
            (raw(), new_entry()).prop_map(|(pick, with)| Recipe::Replace { pick, with }),
            new_entry().prop_map(Recipe::ReplaceUnknown),
            (raw(), raw()).prop_map(|(from, to)| Recipe::Move { from, to }),
            Just(Recipe::Reverse),
            new_entries(4).prop_map(Recipe::ReplaceAll),
            testing::text().prop_map(Recipe::Title),
            testing::text().prop_map(Recipe::Description),
            prop::collection::vec(testing::text(), 0..3).prop_map(Recipe::ResultTitles),
            new_settings().prop_map(Recipe::Settings),
            (testing::picker(), testing::picker_settings())
                .prop_map(|(picker, settings)| Recipe::Picker(picker, settings)),
            testing::picker_settings().prop_map(Recipe::PickerSettings),
        ]
    }

    /// A weight as an edit may bring it: now and then outside [`WEIGHT_RANGE`], to be refused.
    fn weight() -> impl Strategy<Value = u32> {
        prop_oneof![9 => WEIGHT_RANGE, 1 => prop_oneof![Just(0), *WEIGHT_RANGE.end() + 1..]]
    }

    /// An entry as an edit may bring it, its weight from [`weight`].
    fn new_entry() -> impl Strategy<Value = Entry> {
        (testing::entry(), weight()).prop_map(|(entry, weight)| Entry { weight, ..entry })
    }

    /// Up to `max` entries with distinct ids, their weights from [`weight`].
    fn new_entries(max: usize) -> impl Strategy<Value = Vec<Entry>> {
        (testing::entries(0..=max), prop::collection::vec(weight(), max)).prop_map(|(entries, weights)| {
            entries.into_iter().zip(weights).map(|(entry, weight)| Entry { weight, ..entry }).collect()
        })
    }

    /// Settings as an edit may bring them: the duration now and then outside
    /// [`SPIN_DURATION_SECS`], to be refused.
    fn new_settings() -> impl Strategy<Value = ListSettings> {
        let secs = prop_oneof![9 => SPIN_DURATION_SECS, 1 => prop_oneof![Just(0), *SPIN_DURATION_SECS.end() + 1..]];
        (testing::settings(), secs)
            .prop_map(|(settings, spin_duration_secs)| ListSettings { spin_duration_secs, ..settings })
    }

    /// The edit a recipe stands for on `list`. Positions wrap a little past the end of the
    /// list, so that some edits are refused.
    fn edit_for(recipe: Recipe, list: &ListDoc) -> Edit {
        let len = list.entries.len();
        let position = |raw: usize| raw % (len + 2);
        let pick = |raw: usize| list.entries.get(raw % len.max(1)).cloned();
        match recipe {
            Recipe::Insert { at, entries } => Edit::InsertEntries { at: position(at), entries },
            Recipe::InsertCopy { pick: raw, at } => {
                Edit::InsertEntries { at: position(at), entries: pick(raw).into_iter().collect() }
            }
            Recipe::Remove { start, end } => Edit::RemoveEntries(position(start)..position(end)),
            Recipe::Replace { pick: raw, with } => match pick(raw) {
                Some(old) => Edit::ReplaceEntry(Entry { id: old.id, ..with }),
                None => Edit::ReplaceEntry(with),
            },
            Recipe::ReplaceUnknown(entry) => Edit::ReplaceEntry(entry),
            Recipe::Move { from, to } => Edit::MoveEntry { from: position(from), to: position(to) },
            Recipe::Reverse => Edit::ReplaceAllEntries(list.entries.iter().rev().cloned().collect()),
            Recipe::ReplaceAll(entries) => Edit::ReplaceAllEntries(entries),
            Recipe::Title(title) => Edit::SetTitle(title),
            Recipe::Description(description) => Edit::SetDescription(description),
            Recipe::ResultTitles(titles) => Edit::SetResultTitles(titles),
            Recipe::Settings(settings) => Edit::SetSettings(settings),
            Recipe::Picker(picker, settings) => Edit::SetPicker { picker, settings },
            Recipe::PickerSettings(settings) => Edit::SetPickerSettings(settings),
        }
    }

    proptest! {
        /// Undo then redo, as sorte-app does them: the inverses, applied newest first, give
        /// back the original list, and the edits they return give back the edited one. On
        /// the way, a valid list stays valid: an edit that would break it is refused.
        #[test]
        fn inverses_undo_and_redo_any_sequence_of_edits(
            list in testing::list(),
            recipes in prop::collection::vec(recipe(), 0..16),
        ) {
            prop_assert_eq!(list.validate(), Ok(()));
            let mut edited = list.clone();
            let mut undo = Vec::new();
            for recipe in recipes {
                let edit = edit_for(recipe, &edited);
                let before = edited.clone();
                match edit.clone().apply(&mut edited) {
                    Ok(inverse) => {
                        prop_assert_eq!(edited.validate(), Ok(()), "{:?} made the list invalid", edit);
                        undo.push(inverse);
                    }
                    Err(_) => prop_assert_eq!(&edited, &before, "a refused edit changed the list"),
                }
            }
            let done = content(&edited);
            let refused = |step: &str, error: EditError| TestCaseError::fail(format!("{step} refused: {error}"));
            let mut redo = Vec::new();
            for inverse in undo.into_iter().rev() {
                redo.push(inverse.apply(&mut edited).map_err(|error| refused("undo", error))?);
            }
            prop_assert_eq!(content(&edited), content(&list));
            for edit in redo.into_iter().rev() {
                edit.apply(&mut edited).map_err(|error| refused("redo", error))?;
            }
            prop_assert_eq!(content(&edited), done);
        }
    }
}
