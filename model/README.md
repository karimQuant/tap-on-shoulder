# Training pipeline

This directory trains the classifier head that the Rust runtime embeds. `common.py` is the only Python implementation of [`docs/design/contract.md`](../docs/design/contract.md): tokenization, int8 embeddings, features f0–f11, the head input layout, and the decision rule. If you change anything there, bump `feature_spec` and re-export.

## Setup

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install -r model/requirements.txt
scripts/fetch-model.sh     # or copy potion-base-8M into assets/potion-base-8M/ (gitignored)
```

The model files must match `scripts/model-checksums.txt`.

## Train

```bash
python model/train.py --data model/data/synthetic --report
```

- This fits multinomial logistic regression (`class_weight="balanced"`) for both variants: `embed` (524-d) and `features` (12-d, the fallback).
- C is picked from a small grid by 5-fold `GroupKFold` over the `group` field. A group is one generation batch, so no batch is ever in both train and test.
- The script prints macro-F1, per-class recall, and the confusion matrix, with both the argmax rule and the shipped `lean = alert` rule.
- It sweeps `lean_threshold` (0.10–0.50). It keeps the threshold with the best tap-precision among those with tap-recall ≥ 0.95, and `export.py` writes it into `head.json`.
- `--report` is the **preview gate (synthetic)**. It runs on the `embed` grouped-CV predictions under the lean-alert rule at the chosen threshold, and exits 1 unless both of these hold:
  - **tap-recall ≥ 0.95:** the share of true `needs_decision`/`blocked` rows that are predicted as either of those two classes.
  - **4-class macro-F1 ≥ 0.85.**

  It also reports tap-precision and the `needs_decision` recall on its own.
- The fitted heads are saved to `model/artifacts/heads.pkl` (gitignored).

## Export

```bash
python model/export.py
```

| Output | What it is | Committed? |
|---|---|---|
| `assets/head.json` | The `embed` head that ships in the binary | Yes |
| `model/artifacts/head_features.json` | The `features` fallback head (option D) | No |
| `tests/fixtures/parity.json` | Rust parity fixtures | Yes |

The parity fixtures are computed from the exported, rounded weights. They include at least 10 near-threshold cases and the edge cases: empty text, CJK, accents, emoji, a long code block, and a 20 KB text that exercises truncation. Fixtures within 0.01 of any decision boundary are skipped, so f32 summation-order differences can never flip a label.

## Evaluate on real data

```bash
python model/eval.py --holdout path/to/holdout.jsonl
```

It prints PASS or FAIL against the release bar: `needs_decision` recall ≥ 0.95 and macro-F1 ≥ 0.85, on at least 200 real endings that were never used in training.

## Data policy

- **Synthetic data is committed:** `model/data/synthetic/*.jsonl`. Each line is `{"text", "label", "group", "source": "synthetic"}`.
  - `hand-b*` batches are written by hand.
  - `tpl-*` batches come from `model/data/gen_templated.py`. They are capped at 30% of the set.
  - The data contains no real user content, names, hosts, tokens, or paths. It uses placeholders such as `/path/to/app` and `example.com`.
- **Collected and labelled data is never committed.** It stays on the machine that produced it. `.gitignore` blocks `*.jsonl` outside `model/data/synthetic/`.

## Training with your own labelled data

`tap label` writes the reviewed examples to `${CLAUDE_PLUGIN_DATA}/training/labeled.jsonl`. To train with them, add them as extra data:

```bash
python model/train.py --data model/data/synthetic \
  --extra "${CLAUDE_PLUGIN_DATA}/training/labeled.jsonl" --report
python model/export.py
```

- Rows from `--extra` count as `real` in `trained_on`.
- Rows without a `group` field all go into one group per file.
- Keep a separate holdout file for `eval.py`, and never train on it.
