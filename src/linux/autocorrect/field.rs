//! What autocorrect does as the focused field's text changes. It reads only
//! the text before the cursor that the app reports to the input method, and
//! acts on exactly two edits: one word-ending character typed after a word
//! (fix the word), and Backspace right after a fix (put the typed word back
//! and learn it). Anything else (a paste, a jump, a selection) only moves on.
//! A word too close to call waits: once the next word is finished, the word
//! pairs decide it and both are replaced in one edit. It also notices a word
//! Hex fixed or left that you deleted and typed again differently in the same
//! place, and hands that pair to `Learned`.

use super::correct::{Corrector, Scorer, Verdict};
use super::learned::Learned;

/// Characters that finish a word and trigger its correction.
const ENDINGS: &[char] = &[' ', ',', '.', '!', '?', ';', ':', ')'];
/// About how much of the line before a word a language model reads.
const CONTEXT_BYTES: usize = 300;

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
    /// The last word finished: retyped in its place, a fixed or unknown one
    /// teaches a typo, and the next word may still change it.
    finished: Option<Finished>,
}

struct Applied {
    original: String,
    fix: String,
    /// The text before the words.
    prefix: String,
    /// The typed words the fix changed, yours once you take it back.
    changed: Vec<String>,
}

struct Finished {
    /// The text before the word.
    prefix: String,
    typed: String,
    verdict: Verdict,
}

impl Finished {
    /// The word as the field shows it.
    fn shown(&self) -> &str {
        match &self.verdict {
            Verdict::Fix(fix) => fix,
            Verdict::Unknown | Verdict::Leave => &self.typed,
        }
    }
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
        mut scorer: Option<&mut dyn Scorer>,
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
            for word in &applied.changed {
                learned.keep(word);
            }
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
        let verdict = corrector.correct(word, previous_word(prefix), learned);
        let mut word_fix = match &verdict {
            Verdict::Fix(fix) => Some(fix.clone()),
            Verdict::Unknown | Verdict::Leave => None,
        };
        // The word before, when only a space separates it and this word
        // decides it: the language model, else the word pairs for a word
        // left unknown.
        let mut before_fix = None;
        if let Some(finished) = self.finished.take() {
            if finished.prefix == prefix {
                if verdict == Verdict::Leave
                    && finished.verdict != Verdict::Leave
                    && Corrector::slip(&finished.typed, word)
                {
                    learned.retyped(&finished.typed, word);
                }
            } else {
                let unknown = finished.verdict == Verdict::Unknown;
                if prefix == format!("{}{} ", finished.prefix, finished.shown()) {
                    let next = word_fix.as_deref().unwrap_or(word);
                    let previous = previous_word(&finished.prefix);
                    let reviewed = scorer.as_deref_mut().and_then(|scorer| {
                        corrector.review(
                            &finished.typed,
                            previous,
                            context(&finished.prefix),
                            Some(next),
                            learned,
                            scorer,
                        )
                    });
                    before_fix = reviewed
                        .or_else(|| {
                            unknown
                                .then(|| {
                                    corrector.correct_before(
                                        &finished.typed,
                                        previous,
                                        next,
                                        learned,
                                    )
                                })
                                .flatten()
                        })
                        .filter(|fix| fix != finished.shown())
                        .map(|fix| {
                            (
                                fix,
                                finished.prefix.clone(),
                                finished.typed.clone(),
                                finished.shown().to_owned(),
                            )
                        });
                }
                if unknown && before_fix.is_none() {
                    learned.typed(&finished.typed);
                }
            }
        }
        // `word_prefix` is the text before this word once the edit lands.
        let word_prefix = match &before_fix {
            Some((fix, start, ..)) => format!("{start}{fix} "),
            None => prefix.to_owned(),
        };
        // No word follows across punctuation: the model decides this one now.
        if ending != " "
            && let Some(scorer) = scorer
            && let Some(fix) = corrector.review(
                word,
                previous_word(&word_prefix),
                context(&word_prefix),
                None,
                learned,
                scorer,
            )
        {
            word_fix = Some(fix);
        }
        let verdict = match &word_fix {
            Some(fix) => Verdict::Fix(fix.clone()),
            None => verdict,
        };
        // `shown` is what the field holds now, `original` what Backspace
        // puts back.
        let (start, shown, original, fix, changed) = match (before_fix, &word_fix) {
            (None, None) => {
                self.finished = Some(Finished {
                    prefix: word_prefix,
                    typed: word.to_owned(),
                    verdict,
                });
                return None;
            }
            (None, Some(fix)) => (
                prefix.to_owned(),
                word.to_owned(),
                word.to_owned(),
                fix.clone(),
                vec![word.to_owned()],
            ),
            (Some((before, start, typed, shown)), _) => {
                let mut changed = vec![typed.clone()];
                changed.extend(word_fix.is_some().then(|| word.to_owned()));
                let fix = format!("{before} {}", word_fix.as_deref().unwrap_or(word));
                (
                    start,
                    format!("{shown} {word}"),
                    format!("{typed} {word}"),
                    fix,
                    changed,
                )
            }
        };
        self.finished = Some(Finished {
            prefix: word_prefix,
            typed: word.to_owned(),
            verdict,
        });
        self.expect = Some(format!("{start}{fix}{ending}"));
        let edit = Edit {
            delete: shown.len() + ending.len(),
            insert: format!("{fix}{ending}"),
        };
        self.last = Some(Applied {
            original,
            fix,
            prefix: start,
            changed,
        });
        Some(edit)
    }
}

/// The text a language model reads before a word: the end of its line.
fn context(prefix: &str) -> &str {
    let line = prefix.rsplit('\n').next().unwrap_or(prefix).trim_end();
    let mut start = line.len().saturating_sub(CONTEXT_BYTES);
    while !line.is_char_boundary(start) {
        start += 1;
    }
    if start > 0 {
        start = line[start..].find(' ').map_or(start, |at| start + at + 1);
    }
    &line[start..]
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
            if let Some(edit) = field.changed(Some(text), corrector, learned, None) {
                text.truncate(text.len() - edit.delete);
                text.push_str(&edit.insert);
                assert_eq!(
                    field.changed(Some(text), corrector, learned, None),
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
    fn a_word_retyped_the_same_way_twice_is_fixed_the_third_time() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        let mut text = String::new();
        let retype = "realy \u{8}\u{8}ly. ";
        type_into(&mut field, &corrector, &mut learned, &mut text, retype);
        type_into(&mut field, &corrector, &mut learned, &mut text, retype);
        assert_eq!(text, "really. really. ");
        type_into(&mut field, &corrector, &mut learned, &mut text, "realy ");
        assert_eq!(text, "really. really. really ");
    }

    #[test]
    fn a_close_call_is_fixed_once_the_next_word_decides_it() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        let mut text = String::new();
        type_into(&mut field, &corrector, &mut learned, &mut text, "Realy ");
        assert_eq!(text, "Realy ");
        type_into(
            &mut field,
            &corrector,
            &mut learned,
            &mut text,
            "good jsööp ",
        );
        assert_eq!(text, "Really good hallo ");
        type_into(
            &mut field,
            &corrector,
            &mut learned,
            &mut text,
            "realy goood ",
        );
        assert_eq!(text, "Really good hallo really good ", "both in one edit");
        type_into(&mut field, &corrector, &mut learned, &mut text, "\u{8}");
        assert_eq!(text, "Really good hallo realy goood", "and both taken back");
        assert!(learned.knows("realy") && learned.knows("goood"));
    }

    /// A language model that likes only continuations starting with `liked`.
    struct Fake(&'static [&'static str]);

    impl Scorer for Fake {
        fn score(&mut self, _: &str, continuation: &str) -> Option<f32> {
            let first = continuation.split(' ').next()?.to_lowercase();
            Some(if self.0.contains(&first.as_str()) {
                0.0
            } else {
                20.0
            })
        }
    }

    #[test]
    fn a_language_model_fixes_real_words_once_the_next_word_is_typed() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        let mut scorer = Fake(&["ein", "cat"]);
        let mut text = String::new();
        let mut type_into = |text: &mut String, typed: &str| {
            for c in typed.chars() {
                if c == '\u{8}' {
                    text.pop();
                } else {
                    text.push(c);
                }
                if let Some(edit) =
                    field.changed(Some(text), &corrector, &mut learned, Some(&mut scorer))
                {
                    text.truncate(text.len() - edit.delete);
                    text.push_str(&edit.insert);
                    assert_eq!(
                        field.changed(Some(text), &corrector, &mut learned, Some(&mut scorer)),
                        None
                    );
                }
            }
        };
        type_into(&mut text, "The car ");
        assert_eq!(text, "The car ", "a real word waits");
        type_into(&mut text, "purrs ");
        assert_eq!(text, "The cat purrs ");
        type_into(&mut text, "\u{8}");
        assert_eq!(text, "The car purrs", "taken back");
        type_into(&mut text, ". Das ist eim.");
        assert_eq!(text, "The car purrs. Das ist ein.", "decided at once");
        assert!(learned.knows("car"));
    }

    #[test]
    fn pastes_jumps_and_glued_tokens_are_left_alone() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        let mut field = Field::default();
        assert_eq!(
            field.changed(Some("x"), &corrector, &mut learned, None),
            None
        );
        assert_eq!(
            field.changed(Some("x teh "), &corrector, &mut learned, None),
            None,
            "a paste"
        );
        assert_eq!(
            field.changed(Some("see ~/teh"), &corrector, &mut learned, None),
            None
        );
        assert_eq!(
            field.changed(Some("see ~/teh "), &corrector, &mut learned, None),
            None,
            "a path"
        );
        assert_eq!(field.changed(None, &corrector, &mut learned, None), None);
        assert_eq!(
            field.changed(Some("teh "), &corrector, &mut learned, None),
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
        assert!(
            !learned.knows("qzxjv"),
            "the second waits for the next word"
        );
        type_into(&mut field, &corrector, &mut learned, &mut text, "und ");
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
