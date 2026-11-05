//! Entry text: when two texts name the same choice, and how a pasted block becomes entries.

use crate::Entry;

/// The form in which two entry texts are compared ("Hide all instances"): trimmed,
/// lowercased, with every run of whitespace collapsed to one space, so `" Anna  ROSSI"`
/// and `"anna rossi"` count as the same choice.
pub fn normalize_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized.to_lowercase()
}

/// One new entry per non-empty line of `text`, trimmed: what pasting several lines into
/// the list editor adds (F2). Lines may end in `\n`, `\r\n` or `\r`. Weight and hidden
/// markers (PickerWheel's text format) are an import, not a paste: see `sorte-store`.
pub fn entries_from_lines(text: &str) -> Vec<Entry> {
    text.split(['\n', '\r']).map(str::trim).filter(|line| !line.is_empty()).map(Entry::new).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn normalizing_trims_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize_text("  Anna \t ROSSI\n"), "anna rossi");
        assert_eq!(normalize_text("anna rossi"), "anna rossi");
        // A no-break space, as pasted from a web page, is whitespace too.
        assert_eq!(normalize_text("Anna\u{a0}Rossi"), "anna rossi");
        assert_eq!(normalize_text("ÉCOLE Ünïcode"), "école ünïcode");
        assert_eq!(normalize_text(" \t "), "");
    }

    #[test]
    fn each_non_empty_line_becomes_a_trimmed_entry() {
        let texts: Vec<String> = entries_from_lines("  Pizza \r\nSushi\n\n   \n\tKebab al  piatto\rPiadina\n")
            .into_iter()
            .map(|entry| entry.text)
            .collect();
        assert_eq!(texts, ["Pizza", "Sushi", "Kebab al  piatto", "Piadina"]);
    }

    #[test]
    fn pasted_entries_are_new_visible_entries() {
        let entries = entries_from_lines("a\na\nb");
        assert!(entries.iter().all(|entry| entry.enabled && entry.weight == 1 && entry.count == 0));
        let ids: HashSet<_> = entries.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn blank_text_gives_no_entries() {
        assert!(entries_from_lines("").is_empty());
        assert!(entries_from_lines(" \r\n\t\n").is_empty());
    }
}
