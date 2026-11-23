//! What autocorrect does as the focused field's text changes. It reads only
//! the text before the cursor that the app reports to the input method, and
//! acts on exactly two edits: one word-ending character typed after a word
//! (fix the word), and Backspace right after a fix (put the typed word back
//! and learn it). Anything else (a paste, a jump, a selection) only moves on.

use super::correct::{Corrector, Verdict};
use super::learned::Learned;

/// Characters that finish a word and trigger its correction.
const ENDINGS: &[char] = &[' ', ',', '.', '!', '?', ';', ':', ')'];

/// Replace the `delete` bytes before the cursor with `insert`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Edit {
    pub(crate) delete: usize,
    pub(crate) insert: String,
}

#[derive(Default)]
pub(crate) struct Field {
    /// The text before the cursor at the last change.
    before: Option<String>,
    /// What the text before the cursor becomes once the app applies Hex's
    /// edit; its echo is not your typing.
    expect: Option<String>,
    /// The fix just made, while Backspace may still take it back.
    last: Option<Applied>,
}

struct Applied {
    original: String,
    fix: String,
    /// The text before the word.
    prefix: String,
}

impl Field {
    /// The text before the cursor at the last change.
    pub(crate) fn before(&self) -> Option<&str> {
        self.before.as_deref()
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The text before the cursor is now `before` (None while unknown or a
    /// selection is held); the edit to make, if any.
    pub(crate) fn changed(
        &mut self,
        before: Option<&str>,
        corrector: &Corrector,
        learned: &mut Learned,
    ) -> Option<Edit> {
        let Some(now) = before else {
            self.reset();
            return None;
        };
        // Apps report one keystroke more than once.
        if self.before.as_deref() == Some(now) {
            return None;
        }
        if let Some(expect) = &self.expect {
            if expect == now {
                self.expect = None;
                self.before = Some(now.to_owned());
                return None;
            }
            // The app may report the deletion before the insertion.
            if expect.starts_with(now) {
                return None;
            }
        }
        self.expect = None;
        let earlier = self.before.replace(now.to_owned());
        if let Some(applied) = self.last.take()
            && now.strip_prefix(applied.prefix.as_str()) == Some(applied.fix.as_str())
        {
            learned.keep(&applied.original);
            self.expect = Some(format!("{}{}", applied.prefix, applied.original));
            return Some(Edit {
                delete: applied.fix.len(),
                insert: applied.original,
            });
        }
        let ending = now.strip_prefix(earlier?.as_str())?;
        let mut chars = ending.chars();
        if !chars.next().is_some_and(|c| ENDINGS.contains(&c)) || chars.next().is_some() {
            return None;
        }
        let earlier = &now[..now.len() - ending.len()];
        let word = current_word(earlier)?;
        let prefix = &earlier[..earlier.len() - word.len()];
        match corrector.correct(word, previous_word(prefix), learned) {
            Verdict::Fix(fix) => {
                self.expect = Some(format!("{prefix}{fix}{ending}"));
                self.last = Some(Applied {
                    original: word.to_owned(),
                    fix: fix.clone(),
                    prefix: prefix.to_owned(),
                });
                Some(Edit {
                    delete: word.len() + ending.len(),
                    insert: format!("{fix}{ending}"),
                })
            }
            Verdict::Unknown => {
                learned.typed(word);
                None
            }
            Verdict::Leave => None,
        }
    }
}

/// The word that ends `before`, or None inside an address, path, handle, or
/// number.
fn current_word(before: &str) -> Option<&str> {
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic() || *c == '\'')
        .last()
        .map(|(at, _)| at)?;
    let word = before[start..].trim_start_matches('\'');
    let glued = before[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || "@/\\.#_-:~=+&*$%<>[]{}|`".contains(c));
    (!word.is_empty() && !glued).then_some(word)
}

/// The word just before `prefix`'s end, when only a space separates it.
fn previous_word(prefix: &str) -> Option<&str> {
    current_word(prefix.strip_suffix(' ')?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types `text` one character at a time, applying Hex's edits as the app
    /// would, and returns the field's text.
    fn type_into(
        field: &mut Field,
        corrector: &Corrector,
        learned: &mut Learned,
        text: &mut String,
        typed: &str,
    ) {
        for c in typed.chars() {
            if c == '\u{8}' {
                text.pop();
            } else {
                text.push(c);
            }
            if let Some(edit) = field.changed(Some(text), corrector, learned) {
                text.truncate(text.len() - edit.delete);
                text.push_str(&edit.insert);
                assert_eq!(
                    field.changed(Some(text), corrector, learned),
                    None,
                    "the echo"
                );
            }
        }
    }

    #[test]
    fn a_space_fixes_the_word_and_backspace_takes_it_back_for_good() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        let mut text = String::new();
        type_into(
            &mut field,
            &corrector,
            &mut learned,
            &mut text,
            "Das ist abgekürtzt ",
        );
        assert_eq!(text, "Das ist abgekürzt ");
        type_into(
            &mut field,
            &corrector,
            &mut learned,
            &mut text,
            "and brilliant, teh end.",
        );
        assert_eq!(text, "Das ist abgekürzt and brilliant, the end.");
        type_into(&mut field, &corrector, &mut learned, &mut text, " teh ");
        assert_eq!(text, "Das ist abgekürzt and brilliant, the end. the ");
        type_into(&mut field, &corrector, &mut learned, &mut text, "\u{8}");
        assert_eq!(text, "Das ist abgekürzt and brilliant, the end. teh");
        type_into(&mut field, &corrector, &mut learned, &mut text, " teh ");
        assert_eq!(text, "Das ist abgekürzt and brilliant, the end. teh teh ");
    }

    #[test]
    fn pastes_jumps_and_glued_tokens_are_left_alone() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        assert_eq!(field.changed(Some("x"), &corrector, &mut learned), None);
        assert_eq!(
            field.changed(Some("x teh "), &corrector, &mut learned),
            None,
            "a paste"
        );
        assert_eq!(
            field.changed(Some("see ~/teh"), &corrector, &mut learned),
            None
        );
        assert_eq!(
            field.changed(Some("see ~/teh "), &corrector, &mut learned),
            None,
            "a path"
        );
        assert_eq!(field.changed(None, &corrector, &mut learned), None);
        assert_eq!(
            field.changed(Some("teh "), &corrector, &mut learned),
            None,
            "after a selection"
        );
    }

    #[test]
    fn an_unknown_word_typed_twice_is_learned() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        let mut text = String::new();
        type_into(
            &mut field,
            &corrector,
            &mut learned,
            &mut text,
            "qzxjv qzxjv ",
        );
        assert!(learned.knows("qzxjv"));
    }

    #[test]
    fn words_are_found_before_the_cursor() {
        assert_eq!(current_word("so teh"), Some("teh"));
        assert_eq!(current_word("so 'teh"), Some("teh"));
        assert_eq!(current_word("so don't"), Some("don't"));
        assert_eq!(current_word("mail@teh"), None);
        assert_eq!(current_word("v2teh"), None);
        assert_eq!(current_word("so "), None);
        assert_eq!(previous_word("ich "), Some("ich"));
        assert_eq!(previous_word("ich, "), None);
    }
}
