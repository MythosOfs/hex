//! Which word a finished, misspelled word was meant to be. Physical keys give
//! no touch positions, so the typo model is a weighted edit distance: a
//! neighbouring key, a doubled or swapped letter, or a missing accent costs
//! less than any other slip. Every language's model is searched at once (its
//! sorted spellings walked as a trie), so German and English mix in one
//! sentence, and a word any of them knows is never touched.

use super::learned::Learned;
use super::model::Model;

/// Nats one full edit costs against the language model's word cost.
const EDIT_NATS: f32 = 5.0;
/// How much likelier the best reading must be than the next one.
const MARGIN_NATS: f32 = 2.5;
/// No fix into a word rarer than this, edits included.
const MAX_SCORE: f32 = 22.0;
/// A word you taught Hex counts as a moderately common one.
const LEARNED_NATS: f32 = 9.0;
const MAX_LENGTH: usize = 32;

/// Short words, names, and abbreviations people type on purpose.
const SHORTHAND: &[&str] = &[
    "ok", "okay", "thx", "pls", "plz", "tbh", "idk", "imo", "lol", "lmao", "omg", "brb", "btw",
    "afaik", "fyi", "nvm", "irl", "jk", "np", "ngl", "gonna", "wanna", "gotta", "kinda", "sorta",
    "yeah", "yep", "nope", "nah", "hey", "mfg", "lg", "vg", "usw", "bzw", "ggf", "evtl", "zb",
    "hab", "habs", "gibts", "gehts", "nen", "ne", "nem",
];

pub(crate) struct Corrector {
    models: Vec<Model>,
}

/// What to do with a finished word.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Fix(String),
    /// No language knows it and no fix is clear: left as typed.
    Unknown,
    /// A real word, one of yours, or not a word at all: left as typed.
    Leave,
}

struct Reading {
    text: String,
    score: f32,
    capital: bool,
}

impl Corrector {
    pub(crate) fn built_in() -> Self {
        Self {
            models: Model::built_in(),
        }
    }

    /// What to do with `typed` as it is finished. `previous` is the word
    /// before it, if any.
    pub(crate) fn correct(
        &self,
        typed: &str,
        previous: Option<&str>,
        learned: &Learned,
    ) -> Verdict {
        let length = typed.chars().count();
        if !(3..=MAX_LENGTH).contains(&length)
            || !typed.chars().all(|c| c.is_alphabetic() || c == '\'')
            || typed.chars().skip(1).any(char::is_uppercase)
        {
            return Verdict::Leave;
        }
        let lower = typed.to_lowercase();
        if SHORTHAND.contains(&lower.as_str())
            || learned.knows(&lower)
            || self.models.iter().any(|model| model.knows(&lower))
        {
            return Verdict::Leave;
        }
        if let Some(word) = learned.typo(&lower) {
            return Verdict::Fix(match_case(typed, word, false));
        }
        let typed_chars: Vec<char> = lower.chars().collect();
        // One slip in a short word; two only in a long one, where few real
        // words sit that close ("gefixt" is not "Geist").
        let budget = match length {
            ..=7 => 1.0,
            8..=10 => 1.5,
            _ => 2.0,
        };
        let previous = previous.map(str::to_lowercase);
        let mut readings: Vec<Reading> = Vec::new();
        for model in &self.models {
            let previous = previous.as_deref().and_then(|word| model.index(word));
            for (word, edits) in search(model, &typed_chars, budget) {
                let text = model.text(word);
                readings.push(Reading {
                    score: edits * EDIT_NATS + model.cost(word, previous),
                    capital: model.capital_share(&text).is_some_and(|share| share >= 95),
                    text,
                });
            }
        }
        for word in learned.words() {
            let chars: Vec<char> = word.to_lowercase().chars().collect();
            if let Some(edits) = distance(&chars, &typed_chars, budget) {
                readings.push(Reading {
                    text: word.to_owned(),
                    score: edits * EDIT_NATS + LEARNED_NATS,
                    capital: false,
                });
            }
        }
        readings.sort_by(|a, b| a.score.total_cmp(&b.score));
        let Some(best) = readings.first() else {
            return Verdict::Unknown;
        };
        let runner_up = readings
            .iter()
            .find(|reading| reading.text.to_lowercase() != best.text.to_lowercase());
        if best.score > MAX_SCORE
            || runner_up.is_some_and(|other| other.score - best.score < MARGIN_NATS)
        {
            return Verdict::Unknown;
        }
        let fix = match_case(typed, &best.text, best.capital);
        if fix == typed {
            Verdict::Leave
        } else {
            Verdict::Fix(fix)
        }
    }

    /// Whether `word` could be `typo` typed with a slip or two, not another
    /// word written in its place.
    pub(crate) fn slip(typo: &str, word: &str) -> bool {
        let typo: Vec<char> = typo.to_lowercase().chars().collect();
        let word: Vec<char> = word.to_lowercase().chars().collect();
        typo != word && distance(&word, &typo, 2.0).is_some()
    }
}

/// `target` cased as `typed` was: a capital first letter carries over, and a
/// lowercase word the books nearly always capitalise (German nouns, names)
/// takes its capital. A word you taught keeps your spelling.
fn match_case(typed: &str, target: &str, capital: bool) -> String {
    let typed_capital = typed.chars().next().is_some_and(char::is_uppercase);
    if !(typed_capital || capital) {
        return target.to_owned();
    }
    let mut chars = target.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Every model word within `budget` edits of `typed`, with its edits.
fn search(model: &Model, typed: &[char], budget: f32) -> Vec<(u32, f32)> {
    let mut found = Vec::new();
    let first: Vec<f32> = (0..=typed.len()).map(|i| extra_run(typed, i)).collect();
    let mut path = Vec::new();
    let mut rows = vec![first];
    walk(
        model,
        typed,
        budget,
        0,
        model.size(),
        &mut path,
        &mut rows,
        &mut found,
    );
    found
}

/// The cost of `typed[..i]` all typed in excess.
fn extra_run(typed: &[char], i: usize) -> f32 {
    (0..i).map(|at| extra(typed, at)).sum()
}

#[allow(clippy::too_many_arguments)]
fn walk(
    model: &Model,
    typed: &[char],
    budget: f32,
    lo: u32,
    hi: u32,
    path: &mut Vec<char>,
    rows: &mut Vec<Vec<f32>>,
    found: &mut Vec<(u32, f32)>,
) {
    let depth = path.len();
    let mut at = lo;
    while at < hi {
        let code = model.byte(at, depth);
        if code < 0 {
            let edits = rows[depth][typed.len()];
            if edits <= budget {
                found.push((at, edits));
            }
            at += 1;
            continue;
        }
        let end = model.bound(at, hi, depth, code + 1);
        let letter = model.character(code as u8);
        path.push(letter);
        let row = step(typed, path, rows);
        if row.iter().copied().fold(f32::INFINITY, f32::min) <= budget {
            rows.push(row);
            walk(model, typed, budget, at, end, path, rows, found);
            rows.pop();
        }
        path.pop();
        at = end;
    }
}

/// The next edit-distance row once `candidate` (the trie path so far) gained
/// its last letter; `rows` holds the rows before it.
fn step(typed: &[char], candidate: &[char], rows: &[Vec<f32>]) -> Vec<f32> {
    let j = candidate.len();
    let previous = &rows[j - 1];
    let c = candidate[j - 1];
    let mut row = vec![0.0; typed.len() + 1];
    row[0] = previous[0] + missing(candidate, j - 1);
    for i in 1..=typed.len() {
        let t = typed[i - 1];
        let mut best = (previous[i - 1] + substitute(c, t))
            .min(row[i - 1] + extra(typed, i - 1))
            .min(previous[i] + missing(candidate, j - 1));
        if i >= 2 && j >= 2 && c == typed[i - 2] && candidate[j - 2] == t {
            best = best.min(rows[j - 2][i - 2] + 0.7);
        }
        row[i] = best;
    }
    row
}

/// The weighted distance from `candidate` to `typed`, when within `budget`.
fn distance(candidate: &[char], typed: &[char], budget: f32) -> Option<f32> {
    let mut rows = vec![
        (0..=typed.len())
            .map(|i| extra_run(typed, i))
            .collect::<Vec<_>>(),
    ];
    for j in 1..=candidate.len() {
        let row = step(typed, &candidate[..j], &rows);
        if row.iter().copied().fold(f32::INFINITY, f32::min) > budget {
            return None;
        }
        rows.push(row);
    }
    let edits = rows.last()?[typed.len()];
    (edits <= budget).then_some(edits)
}

fn substitute(meant: char, typed: char) -> f32 {
    if meant == typed {
        0.0
    } else if fold(meant) == fold(typed) {
        0.4
    } else if adjacent(meant, typed) {
        0.7
    } else {
        // A key far from the meant one is rarely a slip on a physical
        // keyboard; a short word never gets one ("tounge" is not "lounge").
        1.2
    }
}

/// `typed[i]` typed although not meant: cheaper beside its own double or a
/// neighbouring key (a finger brushing two keys).
fn extra(typed: &[char], i: usize) -> f32 {
    let c = typed[i];
    let near = |other: Option<&char>| other.is_some_and(|&o| o == c || adjacent(o, c));
    if near(i.checked_sub(1).and_then(|p| typed.get(p))) || near(typed.get(i + 1)) {
        0.7
    } else {
        1.0
    }
}

/// `candidate[j]` meant but not typed: cheaper for half of a double letter.
fn missing(candidate: &[char], j: usize) -> f32 {
    if j > 0 && candidate[j - 1] == candidate[j] {
        0.7
    } else {
        1.0
    }
}

fn fold(c: char) -> char {
    match c {
        'ä' | 'à' | 'á' | 'â' => 'a',
        'ö' | 'ò' | 'ó' | 'ô' => 'o',
        'ü' | 'ù' | 'ú' | 'û' => 'u',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'ß' => 's',
        _ => c,
    }
}

/// Whether two letters sit next to each other on QWERTZ or QWERTY.
fn adjacent(a: char, b: char) -> bool {
    const QWERTZ: [&str; 3] = ["qwertzuiopü", "asdfghjklöä", "yxcvbnm"];
    const QWERTY: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
    let place = |layout: &[&str; 3], c: char| {
        layout.iter().enumerate().find_map(|(row, keys)| {
            keys.chars()
                .position(|k| k == c)
                .map(|col| (row as i32, col as i32))
        })
    };
    [QWERTZ, QWERTY]
        .iter()
        .any(|layout| match (place(layout, a), place(layout, b)) {
            (Some((ra, ca)), Some((rb, cb))) => {
                (ra - rb).abs() <= 1 && (ca - cb).abs() <= 1 && (ra, ca) != (rb, cb)
            }
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(corrector: &Corrector, typed: &str, previous: Option<&str>) -> Option<String> {
        match corrector.correct(typed, previous, &Learned::default()) {
            Verdict::Fix(fix) => Some(fix),
            _ => None,
        }
    }

    #[test]
    fn clear_typos_are_fixed_in_either_language() {
        let corrector = Corrector::built_in();
        assert_eq!(
            fix(&corrector, "abgekürtzt", None).as_deref(),
            Some("abgekürzt")
        );
        assert_eq!(
            fix(&corrector, "Abgekürtzt", None).as_deref(),
            Some("Abgekürzt")
        );
        assert_eq!(fix(&corrector, "teh", None).as_deref(), Some("the"));
        assert_eq!(fix(&corrector, "gefixt", None), None, "not Geist");
        assert_eq!(fix(&corrector, "tounge", None), None, "not lounge");
        assert_eq!(fix(&corrector, "becuase", None).as_deref(), Some("because"));
        assert_eq!(
            fix(&corrector, "vielleciht", Some("ich")).as_deref(),
            Some("vielleicht")
        );
    }

    #[test]
    fn real_words_shorthand_and_odd_tokens_stay() {
        let corrector = Corrector::built_in();
        for typed in [
            "brilliant",
            "brillant",
            "Haus",
            "the",
            "lol",
            "NASA",
            "GitHub",
            "ab",
            "x2y",
        ] {
            assert_eq!(fix(&corrector, typed, None), None, "{typed}");
        }
    }

    #[test]
    fn a_learned_word_stays_and_its_typos_are_fixed_to_it() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        assert_ne!(
            corrector.correct("hyprlnad", None, &learned),
            Verdict::Fix("Hyprland".into())
        );
        learned.keep("Hyprland");
        assert_eq!(
            corrector.correct("Hyprland", None, &learned),
            Verdict::Leave
        );
        assert_eq!(
            corrector.correct("hyprland", None, &learned),
            Verdict::Leave
        );
        assert_eq!(
            corrector.correct("Hyprlnad", None, &learned),
            Verdict::Fix("Hyprland".into())
        );
    }

    #[test]
    fn a_retyped_typo_is_fixed_as_you_retyped_it() {
        let corrector = Corrector::built_in();
        let mut learned = Learned::default();
        assert_eq!(corrector.correct("realy", None, &learned), Verdict::Unknown);
        learned.retyped("realy", "really");
        learned.retyped("realy", "really");
        assert_eq!(
            corrector.correct("Realy", None, &learned),
            Verdict::Fix("Really".into())
        );
        assert!(Corrector::slip("realy", "really"));
        assert!(!Corrector::slip("hund", "katze"));
        assert!(!Corrector::slip("Haus", "haus"));
    }

    #[test]
    fn swaps_neighbours_and_doubles_cost_less_than_other_slips() {
        let typed: Vec<char> = "teh".chars().collect();
        let the: Vec<char> = "the".chars().collect();
        assert_eq!(distance(&the, &typed, 2.0), Some(0.7));
        assert!(adjacent('t', 'z') && adjacent('z', 'x') && !adjacent('q', 'p'));
        assert_eq!(substitute('ä', 'a'), 0.4);
    }
}
