# Autocorrect language-model experiment

Would a small local language model fix more physical-keyboard typos than
Labs › Autocorrect's word pairs? Not shipped; nothing in Hex uses it.

```sh
python3 scripts/autocorrect-lm/make-typos.py 300 0   > /tmp/cases.tsv  # tuning set
python3 scripts/autocorrect-lm/make-typos.py 300 300 > /tmp/cases2.tsv # fresh set
HEX_AUTOCORRECT_REPLAY=/tmp/cases2.tsv HEX_AUTOCORRECT_DUMP=/tmp/dump2.tsv \
  cargo test --release --bin hex linux::autocorrect::field::tests::replay -- --ignored --nocapture
g++ -O2 -std=c++17 scripts/autocorrect-lm/score.cpp -lllama -lggml -o /tmp/score
python3 scripts/autocorrect-lm/evaluate.py /tmp/dump2.tsv /tmp/score gemma-3-270m-Q8_0.gguf --rule=1.5,1,6,4,1,all
```

Typos: one slip in about a quarter of the words (neighbouring key, swap,
dropped, extra, doubled, or the hand one key over) in held-out Tatoeba
sentences, 300 German and 300 English per set. The rule was tuned on the first
set and checked on the second.

Fresh set (822 typos, 3,305 correct words), 2026-11-23, RX 9070 XT / 5800X3D:

| | fixed right | fixed wrong | correct words changed |
|---|---:|---:|---:|
| Word pairs (today) | 525 (63.9%) | 23 | 12 (0.36%) |
| + Gemma 3 270M Q8_0, next word, real-word errors | 663 (80.7%) | 26 | 12 (0.36%) |
| + Gemma, next word, no real-word errors | 599 (72.9%) | 26 | 12 |
| + Gemma, at word end | 604 (73.5%) | 27 | 12 |

On the tuning set Qwen3 0.6B (74.5%) and Qwen3 1.7B Q4 (76.1%) did no better
than Gemma 270M (79.3%). The rule: candidates are the engine's readings plus
the typed word itself; score = 1.5 × slip nats + the model's nats for
"candidate next-word" after the sentence so far; a fix must beat the runner-up
by 1 nat and the typed word by 4 (6 when the typed word is a real word).
Letting the typed word compete is what keeps rare correct words (names,
compounds) from being changed. Time per word, all candidates: 17 ms median on
the GPU, 41 ms median / 60 ms p95 on the CPU alone; 290 MB resident (GPU),
400 MB (CPU).
