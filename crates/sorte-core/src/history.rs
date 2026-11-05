//! What follows a draw: the four actions of the result overlay (F5) and the list's history
//! of draws (F6).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Edit, EditError, Entry, EntryId, ListDoc, Outcome, normalize_text};

/// How many draws a list's history keeps; older ones are dropped.
pub const HISTORY_LIMIT: usize = 1000;

/// What the user does with a result (PickerWheel's four result actions).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultAction {
    /// Close the result and change nothing. Closing it with Esc counts as this.
    Done,
    /// Hide the drawn entry, so later draws skip it.
    HideChoice,
    /// Hide every entry that is the same choice as the drawn one: the same text once
    /// normalized (see [`normalize_text`]), whatever its image. Entries with blank text
    /// are therefore all one choice.
    HideAllInstances,
    /// Add one to the drawn entry's count, its score.
    AddCount,
}

/// One past draw, as the Results tab lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawRecord {
    /// When the result was acted on.
    pub at: DateTime<Utc>,
    /// The drawn entry, which may have been deleted since.
    pub entry_id: EntryId,
    /// A copy of the drawn entry's text, so the record stays readable after the entry is
    /// edited or deleted.
    pub text: String,
    /// The id of the picker that staged the draw.
    pub picker: String,
    /// The full arrival order, for ranking pickers.
    pub ranking: Option<Vec<EntryId>>,
    /// What the user did with the result.
    pub action: ResultAction,
}

/// Records a draw in the list's history and applies the chosen action to the entries.
/// `picker` is the id of the picker that staged the draw and `at` the moment the action
/// was chosen; `at` also becomes the list's `updated_at`.
///
/// Returns the edit that undoes the action's changes to the entries, for the undo stack,
/// or `None` when the entries did not change (`Done`, or nothing left to hide or count).
/// The history entry is not part of that edit: the draw happened.
///
/// Fails, leaving the list untouched, when the drawn entry is no longer in the list, or
/// when two entries share an id, as [`draw`](crate::draw) refuses too: the drawn entry
/// could not be told apart. On a list that does not pass [`ListDoc::validate`], the
/// action's edit can also be refused, with the same effect.
pub fn apply_result(
    list: &mut ListDoc,
    outcome: &Outcome,
    action: ResultAction,
    picker: &str,
    at: DateTime<Utc>,
) -> Result<Option<Edit>, EditError> {
    if let Some(id) = list.shared_entry_id() {
        return Err(EditError::DuplicateEntryId(id));
    }
    let selected = list.entry(outcome.selected).ok_or(EditError::UnknownEntry(outcome.selected))?;
    let text = selected.text.clone();
    let undo = action_edit(list, selected, action).map(|edit| edit.apply_at(list, at)).transpose()?;
    list.history.push(DrawRecord {
        at,
        entry_id: outcome.selected,
        text,
        picker: picker.to_owned(),
        ranking: outcome.ranking.clone(),
        action,
    });
    let excess = list.history.len().saturating_sub(HISTORY_LIMIT);
    list.history.drain(..excess);
    list.updated_at = at;
    Ok(undo)
}

/// The edit that carries out `action` on the entries, or `None` if it would change nothing.
fn action_edit(list: &ListDoc, selected: &Entry, action: ResultAction) -> Option<Edit> {
    match action {
        ResultAction::Done => None,
        ResultAction::HideChoice => {
            selected.enabled.then(|| Edit::ReplaceEntry(Entry { enabled: false, ..selected.clone() }))
        }
        ResultAction::AddCount => {
            // A count already at u32::MAX stays there rather than wrapping to zero.
            let count = selected.count.saturating_add(1);
            (count != selected.count).then(|| Edit::ReplaceEntry(Entry { count, ..selected.clone() }))
        }
        ResultAction::HideAllInstances => {
            let choice = normalize_text(&selected.text);
            let mut entries = list.entries.clone();
            let mut hid_any = false;
            for entry in &mut entries {
                if entry.enabled && normalize_text(&entry.text) == choice {
                    entry.enabled = false;
                    hid_any = true;
                }
            }
            hid_any.then_some(Edit::ReplaceAllEntries(entries))
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{AssetRef, testing};

    fn list_of(texts: &[&str]) -> ListDoc {
        let mut list = ListDoc::new("Pranzo");
        list.entries = texts.iter().copied().map(Entry::new).collect();
        list
    }

    fn when(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn winner(list: &ListDoc, index: usize) -> Outcome {
        Outcome { selected: list.entries[index].id, ranking: None }
    }

    fn enabled(list: &ListDoc) -> Vec<bool> {
        list.entries.iter().map(|entry| entry.enabled).collect()
    }

    #[test]
    fn done_records_the_draw_and_changes_no_entry() {
        let list = list_of(&["Pizza", "Sushi"]);
        let mut after = list.clone();
        let outcome =
            Outcome { selected: list.entries[1].id, ranking: Some(vec![list.entries[1].id, list.entries[0].id]) };
        assert_eq!(apply_result(&mut after, &outcome, ResultAction::Done, "horse-race", when(5)), Ok(None));
        assert_eq!(after.entries, list.entries);
        let record = DrawRecord {
            at: when(5),
            entry_id: list.entries[1].id,
            text: "Sushi".into(),
            picker: "horse-race".into(),
            ranking: outcome.ranking,
            action: ResultAction::Done,
        };
        assert_eq!(after.history, [record]);
        assert_eq!(after.updated_at, when(5));
    }

    #[test]
    fn hiding_the_choice_hides_only_the_drawn_entry() {
        let list = list_of(&["Pizza", "pizza", "Sushi"]);
        let mut after = list.clone();
        let undo = apply_result(&mut after, &winner(&list, 0), ResultAction::HideChoice, "wheel", when(0)).unwrap();
        assert_eq!(enabled(&after), [false, true, true]);
        assert_eq!(undo, Some(Edit::ReplaceEntry(list.entries[0].clone())));
    }

    #[test]
    fn hiding_all_instances_hides_the_same_text_whatever_its_case_spacing_or_image() {
        let mut list = list_of(&["Anna Rossi", "Bruno", " anna  ROSSI ", "Anna", "anna rossi", "ANNA\u{a0}ROSSI"]);
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        list.entries[0].image = Some(AssetRef::new(hash, "png").unwrap());
        list.entries[5].enabled = false;
        let mut after = list.clone();
        let undo =
            apply_result(&mut after, &winner(&list, 4), ResultAction::HideAllInstances, "wheel", when(0)).unwrap();
        // Every "anna rossi" however written, the first with an image although the drawn one
        // has none; not "Bruno" or "Anna", other texts.
        assert_eq!(enabled(&after), [false, true, false, true, false, false]);
        assert_eq!(undo, Some(Edit::ReplaceAllEntries(list.entries)));
    }

    #[test]
    fn adding_to_the_count_stops_at_the_maximum() {
        let mut list = list_of(&["Pizza"]);
        list.entries[0].count = 41;
        let mut after = list.clone();
        let undo = apply_result(&mut after, &winner(&list, 0), ResultAction::AddCount, "wheel", when(0)).unwrap();
        assert_eq!(after.entries[0].count, 42);
        assert_eq!(undo, Some(Edit::ReplaceEntry(list.entries[0].clone())));
        after.entries[0].count = u32::MAX;
        let full = after.clone();
        assert_eq!(apply_result(&mut after, &winner(&full, 0), ResultAction::AddCount, "wheel", when(1)), Ok(None));
        assert_eq!(after.entries, full.entries);
        assert_eq!(after.history.len(), 2);
    }

    #[test]
    fn hiding_an_entry_already_hidden_changes_nothing() {
        let mut list = list_of(&["Pizza"]);
        list.entries[0].enabled = false;
        for action in [ResultAction::HideChoice, ResultAction::HideAllInstances] {
            let mut after = list.clone();
            assert_eq!(apply_result(&mut after, &winner(&list, 0), action, "wheel", when(0)), Ok(None));
            assert_eq!(after.entries, list.entries);
        }
    }

    #[test]
    fn the_history_keeps_the_last_thousand_draws() {
        let mut list = list_of(&["Pizza"]);
        let outcome = winner(&list, 0);
        for secs in 0..HISTORY_LIMIT as i64 {
            apply_result(&mut list, &outcome, ResultAction::Done, "wheel", when(secs)).unwrap();
        }
        assert_eq!(list.history.len(), HISTORY_LIMIT);
        assert_eq!(list.history[0].at, when(0));
        apply_result(&mut list, &outcome, ResultAction::Done, "wheel", when(5000)).unwrap();
        assert_eq!(list.history.len(), HISTORY_LIMIT);
        assert_eq!(list.history[0].at, when(1));
        assert_eq!(list.history[HISTORY_LIMIT - 1].at, when(5000));
    }

    #[test]
    fn an_entry_no_longer_in_the_list_is_refused() {
        let list = list_of(&["Pizza"]);
        let mut after = list.clone();
        let gone = EntryId::random();
        let outcome = Outcome { selected: gone, ranking: None };
        assert_eq!(
            apply_result(&mut after, &outcome, ResultAction::Done, "wheel", when(0)),
            Err(EditError::UnknownEntry(gone))
        );
        assert_eq!(after, list);
    }

    #[test]
    fn a_list_whose_entries_share_an_id_is_refused() {
        // A hidden entry with the drawn entry's id, before it: acting on the first entry
        // with that id would record the hidden one's text and hide nothing.
        let mut list = list_of(&["Vecchio", "Nuovo", "Altro"]);
        list.entries[0].enabled = false;
        list.entries[0].id = list.entries[1].id;
        let outcome = winner(&list, 1);
        for action in
            [ResultAction::Done, ResultAction::HideChoice, ResultAction::HideAllInstances, ResultAction::AddCount]
        {
            let mut after = list.clone();
            let refused = apply_result(&mut after, &outcome, action, "wheel", when(0));
            assert_eq!(refused, Err(EditError::DuplicateEntryId(outcome.selected)), "{action:?}");
            assert_eq!(after, list, "{action:?}");
        }
    }

    #[test]
    fn a_refused_action_records_no_draw() {
        // A hidden entry with a weight out of range, which only a document that skipped
        // validate() can hold: the draw ignores it, but the edit hiding every instance
        // would carry it, so the edit is refused and the draw is not recorded.
        let mut list = list_of(&["Pizza", "Sushi"]);
        list.entries[1].enabled = false;
        list.entries[1].weight = 0;
        let mut after = list.clone();
        let refused = apply_result(&mut after, &winner(&list, 0), ResultAction::HideAllInstances, "wheel", when(0));
        assert_eq!(refused, Err(EditError::WeightOutOfRange { entry: list.entries[1].id, weight: 0 }));
        assert_eq!(after, list);
    }

    proptest! {
        #[test]
        fn the_returned_edit_undoes_exactly_the_entry_changes(
            list in testing::list_with(1..=12),
            pick in any::<prop::sample::Index>(),
            action in testing::action(),
            ranked in any::<bool>(),
        ) {
            let selected = &list.entries[pick.index(list.entries.len())];
            let ranking = ranked.then(|| list.entries.iter().map(|entry| entry.id).collect());
            let outcome = Outcome { selected: selected.id, ranking };
            let mut after = list.clone();
            let undo = apply_result(&mut after, &outcome, action, "wheel", when(0)).unwrap();

            let record = DrawRecord {
                at: when(0),
                entry_id: selected.id,
                text: selected.text.clone(),
                picker: "wheel".into(),
                ranking: outcome.ranking.clone(),
                action,
            };
            prop_assert_eq!(after.history.last(), Some(&record));
            prop_assert_eq!(after.history.len(), (list.history.len() + 1).min(HISTORY_LIMIT));
            prop_assert_eq!(undo.is_some(), after.entries != list.entries);
            if let Some(edit) = undo {
                edit.apply(&mut after).unwrap();
            }
            prop_assert_eq!(&after.entries, &list.entries);
        }
    }
}
