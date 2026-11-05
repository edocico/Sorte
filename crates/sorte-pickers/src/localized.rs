//! Text a picker shows in the app's own interface (its name, its settings), in both languages
//! of v1. The app's strings go through Slint's `@tr()`; a picker's travel with the picker, so
//! adding one touches no translation file.

/// A language the interface can be shown in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Italian,
    English,
}

/// The same text in Italian and in English.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalizedStr {
    pub it: &'static str,
    pub en: &'static str,
}

impl LocalizedStr {
    pub const fn new(it: &'static str, en: &'static str) -> Self {
        Self { it, en }
    }

    /// The text in `language`.
    pub fn get(&self, language: Language) -> &'static str {
        match language {
            Language::Italian => self.it,
            Language::English => self.en,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_language_gets_its_own_text() {
        let name = LocalizedStr::new("Ruota", "Wheel");
        assert_eq!(name.get(Language::Italian), "Ruota");
        assert_eq!(name.get(Language::English), "Wheel");
    }
}
