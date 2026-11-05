//! The list as a picker sees it: the visible entries, in order, with what to draw for each.

use peniko::Color;
use sorte_core::{AssetRef, Entry, EntryId, ListDoc, WEIGHT_RANGE};

/// A visible entry, as a picker draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewEntry {
    pub id: EntryId,
    /// The entry's label when it has one that is not blank, else its text.
    pub text: String,
    /// Within `sorte_core::WEIGHT_RANGE`, as the draw counts it.
    pub weight: u32,
    /// Overrides the theme's colour for this entry.
    pub color: Option<Color>,
    pub image: Option<AssetRef>,
}

impl ViewEntry {
    fn of(entry: &Entry) -> Self {
        let label = entry.label.as_deref().filter(|label| !label.trim().is_empty());
        Self {
            id: entry.id,
            text: label.unwrap_or(&entry.text).to_owned(),
            // A document that skipped validation may hold weights the draw refuses; clamped,
            // they cannot give a picker a total of 0 to divide by.
            weight: entry.weight.clamp(*WEIGHT_RANGE.start(), *WEIGHT_RANGE.end()),
            color: entry.color.map(|c| Color::from_rgba8(c.r, c.g, c.b, c.a)),
            image: entry.image.clone(),
        }
    }
}

/// The visible entries of a list, in the list's order: what a picker stages, and exactly the
/// entries `sorte-core`'s draw chooses among.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListView {
    entries: Vec<ViewEntry>,
}

impl ListView {
    pub fn new(list: &ListDoc) -> Self {
        Self { entries: list.visible_entries().map(ViewEntry::of).collect() }
    }

    pub fn entries(&self) -> &[ViewEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Where the entry `id` sits among the visible ones.
    pub fn position(&self, id: EntryId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// The sum of the weights: the W of the draw's odds, weight / W.
    pub fn total_weight(&self) -> u64 {
        self.entries.iter().map(|entry| u64::from(entry.weight)).sum()
    }
}

#[cfg(test)]
mod tests {
    use sorte_core::Rgba8;

    use super::*;

    fn list(entries: Vec<Entry>) -> ListDoc {
        let mut list = ListDoc::new("Pranzo");
        list.entries = entries;
        list
    }

    #[test]
    fn holds_the_visible_entries_in_order() {
        let mut hidden = Entry::new("Sushi");
        hidden.enabled = false;
        let doc = list(vec![Entry::new("Pizza"), hidden, Entry::new("Ramen")]);
        let view = ListView::new(&doc);
        let texts: Vec<&str> = view.entries().iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(texts, ["Pizza", "Ramen"]);
        assert_eq!((view.len(), view.is_empty()), (2, false));
        assert_eq!(view.position(doc.entries[2].id), Some(1));
        assert_eq!(view.position(doc.entries[1].id), None);
        assert!(ListView::new(&list(Vec::new())).is_empty());
    }

    #[test]
    fn a_label_that_is_not_blank_replaces_the_text() {
        let labelled = |label: &str| Entry { label: Some(label.to_owned()), ..Entry::new("Pizza margherita") };
        let doc = list(vec![labelled("Pizza"), labelled(""), labelled("  \t"), Entry::new("Ramen")]);
        let view = ListView::new(&doc);
        let texts: Vec<&str> = view.entries().iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(texts, ["Pizza", "Pizza margherita", "Pizza margherita", "Ramen"]);
    }

    #[test]
    fn carries_weights_colours_and_images() {
        let color = Rgba8 { r: 0x11, g: 0x22, b: 0x33, a: 0x80 };
        let image = AssetRef::new(&"ab".repeat(32), "png").expect("a valid asset reference");
        let doc = list(vec![
            Entry { weight: 7, color: Some(color), image: Some(image.clone()), ..Entry::new("a") },
            Entry { weight: 1000, ..Entry::new("b") },
        ]);
        let view = ListView::new(&doc);
        let [a, b] = view.entries() else { panic!("two entries") };
        assert_eq!(
            (a.weight, a.color, a.image.as_ref()),
            (7, Some(Color::from_rgba8(0x11, 0x22, 0x33, 0x80)), Some(&image))
        );
        assert_eq!((b.weight, b.color, b.image.as_ref()), (1000, None, None));
        assert_eq!(view.total_weight(), 1007);
    }

    #[test]
    fn weights_outside_the_range_are_clamped_into_it() {
        let doc = list(vec![Entry { weight: 0, ..Entry::new("a") }, Entry { weight: 5000, ..Entry::new("b") }]);
        let weights: Vec<u32> = ListView::new(&doc).entries().iter().map(|entry| entry.weight).collect();
        assert_eq!(weights, [1, 1000]);
    }
}
