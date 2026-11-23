#!/usr/bin/env python3
"""Physical-keyboard typos in held-out Tatoeba sentences (evaluation only).

Writes `lang<TAB>reference<TAB>typed` rows: one slip in about a quarter of the
words of 3+ letters (a neighbouring key, a swap, a dropped or extra letter, a
doubled or undoubled letter, or the whole hand one key over), words kept 1:1.
"""
import gzip, random, sys
from pathlib import Path

HELD_OUT = Path(__file__).resolve().parents[2] / "android/benchmark/typing/held-out"
QWERTZ = ["qwertzuiopü", "asdfghjklöä", "yxcvbnm"]
QWERTY = ["qwertyuiop", "asdfghjkl", "zxcvbnm"]


def place(rows, c):
    for r, keys in enumerate(rows):
        if c in keys:
            return r, keys.index(c)
    return None


def neighbours(rows, c):
    at = place(rows, c)
    if not at:
        return []
    out = []
    for r, keys in enumerate(rows):
        for k, key in enumerate(keys):
            if (r, k) != at and abs(r - at[0]) <= 1 and abs(k - at[1]) <= 1:
                out.append(key)
    return out


def slip(word, rows, rng):
    lower = word.lower()
    kind = rng.choices(
        ["near", "swap", "drop", "extra", "double", "shift"], [30, 20, 15, 15, 10, 10]
    )[0]
    i = rng.randrange(len(lower))
    if kind == "near" and neighbours(rows, lower[i]):
        lower = lower[:i] + rng.choice(neighbours(rows, lower[i])) + lower[i + 1 :]
    elif kind == "swap" and len(lower) > 2:
        i = rng.randrange(len(lower) - 1)
        lower = lower[:i] + lower[i + 1] + lower[i] + lower[i + 2 :]
    elif kind == "drop":
        lower = lower[:i] + lower[i + 1 :]
    elif kind == "extra" and neighbours(rows, lower[i]):
        lower = lower[: i + 1] + rng.choice(neighbours(rows, lower[i])) + lower[i + 1 :]
    elif kind == "double":
        doubled = [j for j in range(len(lower) - 1) if lower[j] == lower[j + 1]]
        lower = (
            lower[: doubled[0]] + lower[doubled[0] + 1 :]
            if doubled
            else lower[: i + 1] + lower[i] + lower[i + 1 :]
        )
    elif kind == "shift":
        moved = []
        for c in lower:
            at = place(rows, c)
            if not at or at[1] + 1 >= len(rows[at[0]]):
                return None
            moved.append(rows[at[0]][at[1] + 1])
        lower = "".join(moved)
    else:
        return None
    if lower == word.lower():
        return None
    return lower[0].upper() + lower[1:] if word[0].isupper() else lower


def main():
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 300
    start = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    rng = random.Random(2368 + start)
    for lang, rows in [("de", QWERTZ), ("en", QWERTY)]:
        with gzip.open(HELD_OUT / f"{lang}.tsv.gz", "rt") as f:
            refs = [line.split("\t")[2] for line in f]
        for ref in refs[start : start + count]:
            typed = []
            for word in ref.split(" "):
                bad = None
                if len(word) >= 3 and word.isalpha() and rng.random() < 0.25:
                    bad = slip(word, rows, rng)
                typed.append(bad or word)
            print(f"{lang}\t{ref}\t{' '.join(typed)}")


main()
