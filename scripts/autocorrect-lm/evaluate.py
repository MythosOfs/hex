#!/usr/bin/env python3
"""Experiment: would a small language model fix more typos than Hex's word
pairs? Reads the replay dump (`HEX_AUTOCORRECT_DUMP`), scores each word's
candidates with `score` (libllama) and compares decision rules.

evaluate.py DUMP SCORER MODEL.gguf
"""
import collections, itertools, statistics, subprocess, sys

dump, scorer, model = sys.argv[1:4]
rows = []
for line in open(dump):
    id_, lang, i, left, nxt, ref, typed, final, cands = line.rstrip("\n").split("\t")
    cl = []
    for c in cands.split("|") if cands else []:
        text, edits, score = c.rsplit(":", 2)
        cl.append((text, float(edits)))
    known = [c for c in cl if c[0].lower() == typed.lower() and c[1] == 0]
    # The typed word itself always competes, so the model must find it unlikely.
    own = known or ([(typed.lower(), 0.0)] if cl else [])
    top = cl[:5] + [c for c in own if c not in cl[:5]]
    rows.append(dict(id=id_, lang=lang, left=left, next=nxt, ref=ref, typed=typed, final=final,
                     cands=top, known=bool(known)))


def eligible(r):
    t = r["typed"]
    return len(t) >= 3 and t.isalpha() and not any(c.isupper() for c in t[1:]) and r["cands"]


def cased(word, like):
    return word[0].upper() + word[1:] if like[0].isupper() else word


queries = []
for n, r in enumerate(rows):
    if not eligible(r):
        continue
    for text, _ in r["cands"]:
        c = cased(text, r["typed"])
        queries.append((f"{n}|{text}|0", r["left"], c))
        if r["next"]:
            queries.append((f"{n}|{text}|1", r["left"], f"{c} {r['next']}"))
queries.sort(key=lambda q: q[1])
cache = f"{dump}.{model.rsplit('/', 1)[-1]}.lm"
try:
    out = open(cache).read()
except FileNotFoundError:
    out = subprocess.run([scorer, model], input="".join(f"{a}\t{b}\t{c}\n" for a, b, c in queries),
                         capture_output=True, text=True, check=True).stdout
    open(cache, "w").write(out)
lm, cost = {}, collections.Counter()
for line in out.splitlines():
    qid, nats, us = line.split("\t")
    n, text, mode = qid.split("|")
    lm[(int(n), text, int(mode))] = float(nats)
    cost[(int(n), int(mode))] += int(us)


def decide(n, r, weight, margin, real, unknown, mode):
    """The rule's word for row n, or None to keep the engine's."""
    use = mode if r["next"] else 0
    scored = sorted((weight * e + lm[(n, t, use)], t, e) for t, e in r["cands"])
    (best, text, edits), rest = scored[0], scored[1:]
    if rest and rest[0][0] - best < margin:
        return None
    if text.lower() == r["typed"].lower():
        return None
    if r["known"] and real is None:
        return None
    own = next(s for s, t, _ in scored if t.lower() == r["typed"].lower())
    if own - best < (real if r["known"] else unknown):
        return None
    return cased(text, r["typed"])


def run(rule, show_harm=False):
    typos = right = wrong = clean = harmed = 0
    for n, r in enumerate(rows):
        word = r["final"]
        if eligible(r) and rule is not None:
            weight, margin, real, unknown, mode, scope = rule
            engine_untouched = word.lower() == r["typed"].lower()
            if scope == "all" or engine_untouched:
                word = decide(n, r, weight, margin, real, unknown, mode) or word
        same = lambda a, b: a.lower() == b.lower()
        if same(r["typed"], r["ref"]):
            clean += 1
            harmed += not same(word, r["ref"])
            if show_harm and not same(word, r["ref"]):
                print(f"    changed: {r['left'][-30:]} [{r['ref']} -> {word}] {r['next']}")
        else:
            typos += 1
            right += same(word, r["ref"])
            wrong += not same(word, r["ref"]) and not same(word, r["typed"])
    return typos, right, wrong, clean, harmed


def show(name, result):
    typos, right, wrong, clean, harmed = result
    print(f"{name:52} right {right:4} ({right / typos:5.1%})  wrong {wrong:3}  left {typos - right - wrong:4}  "
          f"correct words changed {harmed:3} ({harmed / clean:.2%})")


print(model.rsplit("/", 1)[-1])
show("engine today", run(None))
fixed = [a for a in sys.argv if a.startswith("--rule=")]
if fixed:
    w, m, real, unk, mode, scope = fixed[0][7:].split(",")
    rule = (float(w), float(m), None if real == "-" else float(real), float(unk), int(mode), scope)
    show(f"fixed rule {fixed[0][7:]}", run(rule, show_harm="--harm" in sys.argv))
    sys.exit()
best = {}
for scope, real in [("left", None), ("all", None), ("all", "real")]:
    for weight, margin, mode, t, u in itertools.product(
            [0.6, 1.0, 1.5], [1, 2, 3, 4, 6], [0, 1], [6, 8, 10, 12, 15] if real else [None], [0, 2, 4, 6, 8]):
        result = run((weight, margin, t, u, mode, scope))
        typos, right, wrong, clean, harmed = result
        merit = right - 3 * wrong - 5 * harmed
        key = (scope, real, mode)
        if key not in best or merit > best[key][0]:
            best[key] = (merit, (weight, margin, t, u), result)
for (scope, real, mode), (_, params, result) in sorted(best.items(), key=str):
    label = {"left": "model decides words the engine left",
             "all": "model re-ranks every unknown word"}[scope]
    label += " + real-word errors" if real else ""
    label += ", waits for next word" if mode else ", at word end"
    show(f"{label}", result)
    print(f"{'':52} (typo weight {params[0]}, margin {params[1]}, real-word margin {params[2]}, own margin {params[3]})")
if "--harm" in sys.argv:
    for (scope, real, mode), (_, params, result) in best.items():
        if scope == "all" and real and mode:
            run((params[0], params[1], params[2], params[3], mode, scope), show_harm=True)
per_word = [cost[(n, 1 if r["next"] else 0)] / 1000 for n, r in enumerate(rows) if eligible(r)]
print(f"time per word (all candidates): median {statistics.median(per_word):.1f} ms, "
      f"p95 {sorted(per_word)[int(len(per_word) * 0.95)]:.1f} ms, max {max(per_word):.1f} ms")
