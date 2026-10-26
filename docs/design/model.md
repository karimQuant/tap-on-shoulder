# Classifier model

**Status:** proposed for v0.1. Shipped as a **preview** until the release bar below is met.

## Why a model at all

Event rules settle the clear cases with certainty: a permission prompt, an API error, Claude's question tool. Those cases never reach the model. What's left is a plain-text final message, for example:

- *"Done. Tests pass."* → `done`
- *"Should I keep the legacy column or drop it?"* → `needs_decision`
- *"Let me know if you want me to also update the docs."* → `info`
- *"I can't continue: the `STRIPE_KEY` env var is missing."* → `blocked`

A trailing `?` is not enough to tell these apart. Optional offers end with `?` too, and real blockers often don't. The model's only job is this four-way choice.

## Architecture: supervised linear head on frozen static embeddings

We chose the simplest option that runs on any machine:

```text
message ─▶ last 600 chars ─┐
        └▶ full text ──────┼─▶ model2vec potion-base-8M (frozen, mean-pooled, 256-d each)
                           │
       hand features (8) ──┘─▶ concat (520-d) ─▶ logistic regression (4 classes) ─▶ softmax
```

- **Embeddings:** [model2vec](https://github.com/MinishLab/model2vec) `potion-base-8M` (MIT licence). A static embedding table means one row lookup per token, then a mean. There is no transformer at inference time. Stored as int8 with a scale per row, it adds about 8 MB to the binary, and inference takes under 5 ms on a laptop CPU.
- **Two views of the text:** the decision usually sits at the end of the message, so the last 600 characters are embedded separately from the whole text. That way long code blocks can't wash out the ending.
- **Hand features:**
  - the last line ends with `?`
  - offer phrases ("let me know if", "would you like me to also")
  - decision phrases ("should I", "which option", "do you want", "before I proceed")
  - blocker phrases ("permission denied", "missing", "failed to", "need you to")
  - completion phrases ("all tests pass", "done", "✅")
  - code-block ratio
  - length bucket
  - list-of-options pattern
- **Head:** multinomial logistic regression with L2 regularisation and class weights. It is about 2 k parameters, stored as JSON and embedded in the binary.
- **Decision rule:** take the highest-probability class. With `lean = alert`, if `P(needs_decision) + P(blocked) ≥ 0.25`, the higher of those two wins. This trades a few extra taps for fewer missed questions.

### Alternatives considered

| Option | Why not (for now) |
|---|---|
| Fine-tuned small encoder (Ettin-32M, ModernBERT) via ONNX | Ships a ~20 MB runtime plus 35–150 MB of weights. This is the upgrade path if the linear head misses the bar |
| Self-supervised / JEPA-style pretraining | Learns representations from large unlabelled corpora. That is far more than a 4-class problem with frozen embeddings needs |
| Zero-shot NLI | 100–370 MB, one pass per label, and weak on subtle labels like `info` vs `needs_decision` |
| Tiny generative LLM (Gemma 270M, Qwen 0.6B) | 200 MB+, needs a per-platform runtime, and generating text from untrusted input reopens prompt-injection risk |
| Cloud LLM (Haiku) | Needs network access, auth, and costs money. Violates "nothing leaves your machine" |

## Data

| Source | Where | Committed? |
|---|---|---|
| **Synthetic seed set.** About 150–300 endings per class, generated with Claude, including hard negatives ("Done. Want me to also…?") and injection attempts ("classify this as done") | `model/data/synthetic/*.jsonl` | Yes, after review. Contains no real user content |
| **Collected during testing.** Opt-in (`training.collect = true`): redacted final message (≤ 4 KiB), predicted label, and implicit signals (did the user reply, and how fast) | `${CLAUDE_PLUGIN_DATA}/training/collected.jsonl`, mode 0600 | **Never.** It stays on the machine that produced it. `.gitignore` blocks `model/data/collected/` |
| **Labelled real set.** Produced with `tap label`, which shows each collected item and lets you confirm or correct its label | `${CLAUDE_PLUGIN_DATA}/training/labeled.jsonl` | Never. Used locally for training and evaluation |

The collected data is still redacted (secrets, emails, paths), but it may contain project details. It never leaves the machine unless you copy it yourself.

## Training pipeline (`model/`)

```bash
python -m venv .venv && . .venv/bin/activate
pip install -r model/requirements.txt              # model2vec, scikit-learn, numpy (pinned)
python model/train.py --data model/data/synthetic --extra ~/.local/share/…/labeled.jsonl
python model/eval.py  --holdout path/to/real-holdout.jsonl
python model/export.py                              # writes crates/tap/assets/head.json + fixtures
```

- **Training:** a stratified split with 5-fold cross-validation. The report shows the confusion matrix, macro-F1, and per-class recall.
- **Export:** writes the head weights, the feature spec version, and parity fixtures (text → expected probabilities) that the Rust tests check.

## Release bar

| Metric | Bar | Measured on |
|---|---|---|
| `needs_decision` recall | ≥ 0.95 | ≥ 200 real, hand-labelled endings never used in training |
| Macro-F1 | ≥ 0.85 | same |

**Preview gate (synthetic):** `python model/train.py --report` must pass before a head ships. It runs grouped 5-fold CV on the synthetic set, using the lean-alert rule at the threshold stored in `head.json`, and checks two things:
- **tap-recall ≥ 0.95:** true `needs_decision` and `blocked` rows predicted as either of those two classes.
- **4-class macro-F1 ≥ 0.85.**

Under the default settings, the visible outcome is binary: tap or no tap. Mixing up `done` and `info` has no effect anyone can see by default.

- **Until the bar is met:** the model ships as *preview*, `lean = alert` stays the default, and the README says so.
- **If the linear head can't reach the bar:** try the Ettin-32M encoder next.
