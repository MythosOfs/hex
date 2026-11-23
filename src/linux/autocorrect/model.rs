//! The phone keyboards' typing models, read in place: HXTM v3 (sorted
//! letter-code spellings, quantized unigram costs, and CSR word-pair rows),
//! the `<lang>.valid` list of real words outside the model, and the
//! `<lang>.capitals` list. The same files Android ships
//! (`android/app/src/main/assets/typing`, `TypingModel.kt`); nothing is copied
//! into a heap dictionary.

use std::collections::HashMap;

const MAGIC: u32 = 0x4d54_5848;

pub(crate) struct Model {
    data: &'static [u8],
    words: u32,
    codes: HashMap<char, u8>,
    characters: Vec<char>,
    starts: usize,
    uni: usize,
    rows: usize,
    seconds: usize,
    bi: usize,
    blob: usize,
    valid: &'static [u8],
    capitals: &'static [u8],
}

impl Model {
    /// German and English, built into Hex.
    pub(crate) fn built_in() -> Vec<Model> {
        macro_rules! asset {
            ($name:literal) => {
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/android/app/src/main/assets/typing/",
                    $name
                ))
            };
        }
        vec![
            Model::open(
                asset!("de.hextm"),
                "de",
                asset!("de.valid"),
                asset!("de.capitals"),
            ),
            Model::open(
                asset!("en.hextm"),
                "en",
                asset!("en.valid"),
                asset!("en.capitals"),
            ),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn open(
        data: &'static [u8],
        language: &'static str,
        valid: &'static [u8],
        capitals: &'static [u8],
    ) -> Option<Self> {
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
        };
        if u32_at(0)? != MAGIC || u32_at(4)? != 3 {
            return None;
        }
        let words = u32_at(8)?;
        let pairs = u32_at(12)? as usize;
        let blob_bytes = u32_at(16)? as usize;
        let alphabet = u32_at(20)?;
        if !(1..=250_000).contains(&words) || !(1..=250).contains(&alphabet) {
            return None;
        }
        let tag: String = data[24..28]
            .iter()
            .filter(|byte| **byte != 0)
            .map(|byte| *byte as char)
            .collect();
        if tag != language {
            return None;
        }
        let mut codes = HashMap::new();
        let mut characters = vec!['\0'; alphabet as usize + 1];
        for i in 0..alphabet as usize {
            let at = 28 + i * 8;
            let character = char::from_u32(u32_at(at)?)?;
            let code = *data.get(at + 4)?;
            if code == 0 || u32::from(code) > alphabet {
                return None;
            }
            codes.insert(character, code);
            characters[code as usize] = character;
        }
        let starts = 28 + 8 * alphabet as usize;
        let uni = starts + 4 * (words as usize + 1);
        let rows = (uni + words as usize + 3) & !3;
        let seconds = rows + 4 * (words as usize + 1);
        let bi = seconds + 4 * pairs;
        let blob = bi + pairs;
        if blob + blob_bytes != data.len()
            || valid.last().is_some_and(|byte| *byte != b'\n')
            || capitals.last().is_some_and(|byte| *byte != b'\n')
        {
            return None;
        }
        Some(Self {
            data,
            words,
            codes,
            characters,
            starts,
            uni,
            rows,
            seconds,
            bi,
            blob,
            valid,
            capitals,
        })
    }

    fn u32_at(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap())
    }

    fn start(&self, word: u32) -> usize {
        self.u32_at(self.starts + 4 * word as usize) as usize
    }

    /// The letter code of `character`, 0 when the model has none.
    pub(crate) fn code(&self, character: char) -> u8 {
        self.codes.get(&character).copied().unwrap_or(0)
    }

    pub(crate) fn character(&self, code: u8) -> char {
        self.characters[code as usize]
    }

    pub(crate) fn encode(&self, word: &str) -> Option<Vec<u8>> {
        word.chars()
            .map(|character| Some(self.code(character)).filter(|code| *code != 0))
            .collect()
    }

    pub(crate) fn size(&self) -> u32 {
        self.words
    }

    /// The letter code at `depth`, or -1 past the word's end.
    pub(crate) fn byte(&self, word: u32, depth: usize) -> i32 {
        let at = self.start(word) + depth;
        if at < self.start(word + 1) {
            i32::from(self.data[self.blob + at])
        } else {
            -1
        }
    }

    pub(crate) fn text(&self, word: u32) -> String {
        (self.start(word)..self.start(word + 1))
            .map(|at| self.characters[self.data[self.blob + at] as usize])
            .collect()
    }

    /// The first word in `[lo, hi)` whose letter at `depth` is at least
    /// `target` (they share their first `depth` letters).
    pub(crate) fn bound(&self, mut lo: u32, mut hi: u32, depth: usize, target: i32) -> u32 {
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.byte(mid, depth) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub(crate) fn index(&self, word: &str) -> Option<u32> {
        let codes = self.encode(word)?;
        let (mut lo, mut hi) = (0, self.words);
        for (depth, code) in codes.iter().enumerate() {
            lo = self.bound(lo, hi, depth, i32::from(*code));
            hi = self.bound(lo, hi, depth, i32::from(*code) + 1);
            if lo == hi {
                return None;
            }
        }
        (self.byte(lo, codes.len()) == -1).then_some(lo)
    }

    /// -ln P(word | previous) in nats, with the unigram backoff.
    pub(crate) fn cost(&self, word: u32, previous: Option<u32>) -> f32 {
        let unigram = self.data[self.uni + word as usize];
        let backoff = 0.92 + f32::from(unigram) * 0.125;
        match previous.and_then(|previous| self.pair(previous, word)) {
            Some(pair) => backoff.min(f32::from(self.data[self.bi + pair]) * 0.125),
            None => backoff,
        }
    }

    fn pair(&self, previous: u32, word: u32) -> Option<usize> {
        let row = |at: u32| self.u32_at(self.rows + 4 * at as usize) as usize;
        let (mut lo, end) = (row(previous), row(previous + 1));
        let mut hi = end;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.u32_at(self.seconds + 4 * mid) < word {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo < end && self.u32_at(self.seconds + 4 * lo) == word).then_some(lo)
    }

    /// Whether `word` (lowercase) is a real word of this language: in the
    /// model, its validity list, or its capitals list (names).
    pub(crate) fn knows(&self, word: &str) -> bool {
        self.index(word).is_some()
            || line_value(self.valid, word).is_some()
            || self.capital_share(word).is_some()
    }

    /// How often the books write `word` with a capital, 50–100 percent.
    pub(crate) fn capital_share(&self, word: &str) -> Option<u32> {
        line_value(self.capitals, word)?.parse().ok()
    }
}

/// Binary-searches a newline-delimited list sorted by UTF-8 bytes, each line
/// a key alone or `key<TAB>value`; the value (empty when absent) of `key`.
fn line_value(list: &'static [u8], key: &str) -> Option<&'static str> {
    let key = key.as_bytes();
    let (mut lo, mut hi) = (0, list.len());
    while lo < hi {
        let mut at = (lo + hi) / 2;
        while at > lo && list[at - 1] != b'\n' {
            at -= 1;
        }
        let end = at + list[at..].iter().position(|byte| *byte == b'\n')?;
        let line = &list[at..end];
        let tab = line
            .iter()
            .position(|byte| *byte == b'\t')
            .unwrap_or(line.len());
        match line[..tab].cmp(key) {
            std::cmp::Ordering::Equal => {
                let value = line.get(tab + 1..).unwrap_or_default();
                return std::str::from_utf8(value).ok();
            }
            std::cmp::Ordering::Less => lo = end + 1,
            std::cmp::Ordering::Greater => hi = at,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_models_open_and_know_their_words() {
        let models = Model::built_in();
        assert_eq!(models.len(), 2);
        let (de, en) = (&models[0], &models[1]);
        assert!(de.knows("abgekürzt"));
        assert!(!de.knows("abgekürtzt"));
        assert!(en.knows("brilliant"));
        assert!(en.knows("the"));
        assert!(!en.knows("teh"));
        let the = en.index("the").unwrap();
        assert_eq!(en.text(the), "the");
        assert!(en.cost(the, None) < 6.0);
    }

    #[test]
    fn a_sorted_list_answers_its_keys_and_values() {
        let list: &'static [u8] = b"apfel\t60\nbaum\nzug\t99\n";
        assert_eq!(line_value(list, "apfel"), Some("60"));
        assert_eq!(line_value(list, "baum"), Some(""));
        assert_eq!(line_value(list, "zug"), Some("99"));
        assert_eq!(line_value(list, "auto"), None);
        assert_eq!(line_value(list, "zz"), None);
    }
}
