//! The words autocorrect learned from you, as the phone keyboards learn: a
//! word you took a correction back from is yours at once, and a word Hex
//! didn't know becomes yours once you've typed it twice. Learned words are
//! never corrected, and typos of them are fixed to them. A typo you retyped
//! the same way twice is fixed that way from then on. Only these words and
//! pairs are kept, owner-only in `autocorrect.json`; nothing else you type is
//! stored.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Times an unknown word is typed before Hex treats it as a word.
const TYPED_TO_LEARN: u32 = 2;
/// The most words kept; the least used go first.
const LIMIT: usize = 5_000;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Learned {
    #[serde(default)]
    words: BTreeMap<String, Entry>,
    /// Lowercased typo → the word you retyped it as.
    #[serde(default)]
    typos: BTreeMap<String, Typo>,
    /// Counts changes since loading, so the owner knows when to save.
    #[serde(skip)]
    generation: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct Entry {
    /// As you last typed it.
    text: String,
    typed: u32,
    /// You took a correction of it back.
    kept: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct Typo {
    word: String,
    retyped: u32,
}

impl Entry {
    fn learned(&self) -> bool {
        self.kept || self.typed >= TYPED_TO_LEARN
    }
}

impl Learned {
    /// The saved words; a missing or unreadable file starts empty.
    pub(crate) fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "autocorrect's learned words are unreadable; starting over");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::durable::write_private(path, &json)
    }

    /// Whether `lower` is a word you taught Hex.
    pub(crate) fn knows(&self, lower: &str) -> bool {
        self.words.get(lower).is_some_and(Entry::learned)
    }

    /// The word you retyped `lower` as, once you've done so twice.
    pub(crate) fn typo(&self, lower: &str) -> Option<&str> {
        self.typos
            .get(lower)
            .filter(|typo| typo.retyped >= TYPED_TO_LEARN)
            .map(|typo| typo.word.as_str())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The learned words as you spell them.
    pub(crate) fn words(&self) -> impl Iterator<Item = &str> {
        self.words
            .values()
            .filter(|entry| entry.learned())
            .map(|entry| entry.text.as_str())
    }

    /// You took a correction of `word` back: it is yours from now on.
    pub(crate) fn keep(&mut self, word: &str) {
        self.typos.remove(&word.to_lowercase());
        self.entry(word).kept = true;
    }

    /// You deleted `typo`, which Hex fixed or left, and typed `word` in its
    /// place.
    pub(crate) fn retyped(&mut self, typo: &str, word: &str) {
        self.generation += 1;
        let (typo, lower) = (typo.to_lowercase(), word.to_lowercase());
        // Not one of your words after all.
        self.words.remove(&typo);
        // You fixed it the other way round before: neither is a typo.
        if self
            .typos
            .get(&lower)
            .is_some_and(|other| other.word.to_lowercase() == typo)
        {
            self.typos.remove(&lower);
            return;
        }
        if self.typos.len() >= LIMIT && !self.typos.contains_key(&typo) {
            let least = self
                .typos
                .iter()
                .min_by_key(|(_, typo)| typo.retyped)
                .map(|(key, _)| key.clone());
            if let Some(least) = least {
                self.typos.remove(&least);
            }
        }
        let entry = self.typos.entry(typo).or_insert_with(|| Typo {
            word: word.to_owned(),
            retyped: 0,
        });
        // Retyped as another word: start counting that one.
        if entry.word.to_lowercase() != lower {
            entry.retyped = 0;
        }
        // A capital at a sentence's start isn't your spelling.
        if entry.word.to_lowercase() != lower || word == lower {
            entry.word = word.to_owned();
        }
        entry.retyped = entry.retyped.saturating_add(1);
    }

    /// Hex left `word`, which no language knows, as you typed it.
    pub(crate) fn typed(&mut self, word: &str) {
        let entry = self.entry(word);
        entry.typed = entry.typed.saturating_add(1);
    }

    fn entry(&mut self, word: &str) -> &mut Entry {
        self.generation += 1;
        if self.words.len() >= LIMIT && !self.words.contains_key(&word.to_lowercase()) {
            let least = self
                .words
                .iter()
                .min_by_key(|(_, entry)| (entry.kept, entry.typed))
                .map(|(key, _)| key.clone());
            if let Some(least) = least {
                self.words.remove(&least);
            }
        }
        let entry = self
            .words
            .entry(word.to_lowercase())
            .or_insert_with(|| Entry {
                text: word.to_owned(),
                typed: 0,
                kept: false,
            });
        // A capital at a sentence's start isn't your spelling; one inside is.
        if word.chars().skip(1).any(char::is_uppercase) || entry.text.to_lowercase() == entry.text {
            entry.text = word.to_owned();
        }
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_back_word_is_learned_at_once_and_a_typed_one_on_its_second_time() {
        let mut learned = Learned::default();
        learned.keep("Hyprland");
        assert!(learned.knows("hyprland"));
        learned.typed("waybar");
        assert!(!learned.knows("waybar"));
        learned.typed("waybar");
        assert!(learned.knows("waybar"));
        let mut words: Vec<&str> = learned.words().collect();
        words.sort_unstable();
        assert_eq!(words, ["Hyprland", "waybar"]);
    }

    #[test]
    fn learned_words_survive_a_save_and_an_unreadable_file_starts_empty() {
        let dir = std::env::temp_dir().join(format!("hex-autocorrect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("autocorrect.json");
        let mut learned = Learned::default();
        learned.keep("GitHub");
        learned.save(&path).unwrap();
        assert!(Learned::load(&path).knows("github"));
        std::fs::write(&path, b"not json").unwrap();
        assert!(!Learned::load(&path).knows("github"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_typo_retyped_the_same_way_twice_is_learned() {
        let mut learned = Learned::default();
        learned.typed("realy");
        learned.retyped("Realy", "Really");
        assert_eq!(learned.typo("realy"), None);
        learned.retyped("realy", "really");
        assert_eq!(learned.typo("realy"), Some("really"));
        assert!(!learned.knows("realy"));
        learned.retyped("realy", "relay");
        assert_eq!(learned.typo("realy"), None, "retyped another way");
        learned.retyped("realy", "relay");
        assert_eq!(learned.typo("realy"), Some("relay"));
        learned.keep("realy");
        assert_eq!(learned.typo("realy"), None, "taken back");

        learned.retyped("teh", "the");
        learned.retyped("teh", "the");
        learned.retyped("the", "teh");
        assert_eq!(learned.typo("teh"), None, "fixed both ways");
        assert_eq!(learned.typo("the"), None);
    }

    #[test]
    fn the_least_used_word_makes_room() {
        let mut learned = Learned::default();
        for i in 0..LIMIT {
            learned.keep(&format!("word{i}"));
        }
        learned.typed("rare");
        assert_eq!(learned.words.len(), LIMIT);
        assert!(learned.words.contains_key("rare"));
    }
}
