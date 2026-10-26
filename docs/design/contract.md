# Implementation contract (v1)

This page is the single source of truth shared by the Rust runtime (`src/`) and the training pipeline (`model/`). If you change anything here, bump `feature_spec` and regenerate the `head.json` and parity fixtures.

## Categories

The order is fixed. It is also the head's output order.

| idx | id | Taps by default |
|---|---|---|
| 0 | `needs_decision` | yes |
| 1 | `blocked` | yes |
| 2 | `done` | only if `alerts.completion` |
| 3 | `info` | only if `alerts.info` |

`needs_permission` comes only from events (`PermissionRequest`). The model never predicts it.

## Text preparation

- `text`: `last_assistant_message`, truncated to its first 16 384 Unicode scalar values.
- `tail`: the last 600 scalar values of `text.trim_end()`.

## Tokenization (BERT WordPiece, as in `potion-base-8M/tokenizer.json`)

1. **Clean** (as in HF `BertNormalizer`). Drop U+0000, U+FFFD, and every character in Unicode categories `C*` other than `\t\n\r`. Map `\t\n\r`, every `char::is_whitespace` character, and every `Zs` character to a space.
2. **CJK.** Surround every CJK ideograph with spaces. These are the BERT ranges: 4E00–9FFF, 3400–4DBF, 20000–2A6DF, 2A700–2B73F, 2B740–2B81F, 2B820–2CEAF, F900–FAFF, 2F800–2FA1F.
3. **Strip accents, then lowercase.** This is HF's order. Apply NFD, drop category `Mn`, then lowercase. (The tokenizer has `strip_accents: null` with `lowercase: true`, which means accents are stripped.)
4. **Pre-tokenize.** Split on whitespace. Then split each piece around every punctuation character, keeping the punctuation as its own token. Punctuation is ASCII 33–47, 58–64, 91–96, 123–126, plus any Unicode category `P*`.
5. **WordPiece.** A word longer than 100 characters becomes `[UNK]`. Otherwise, match greedily from left to right, taking the longest match each time, with continuation pieces prefixed `##`. If any piece fails to match, the whole word becomes `[UNK]`.
6. **Ids.** Drop the tokenizer's special `added_tokens`. In potion-base-8M these are `[PAD]` 0, `[UNK]` 1, `[CLS]` 2, `[SEP]` 3, `[MASK]` 4. (IDs 100–103 are ordinary wordpieces in this vocab.) Keep the **first 512** ids.

The ground truth is the HF `tokenizers` library loading the same `tokenizer.json` with `add_special_tokens=False`, followed by step 6.

## Embedding

- **Source:** `minishlab/potion-base-8M` at revision `bf8b056651a2c21b8d2565580b8569da283cab23`. `model.safetensors` holds tensor `embeddings`, F32, shape [29528, 256]. The full sha256 values for `model.safetensors` and `tokenizer.json` are pinned in `scripts/model-checksums.txt`, and `scripts/fetch-model.sh` verifies them.
- **int8 per-row quantization:**
  - `scale_r = max(|row_r|) / 127` (f32). If `max` is 0, then `scale_r = 1`.
  - `q = clamp(round_half_away_from_zero(x / scale_r), -127, 127)`.
  - Dequantize as `q * scale_r`.
- **Sentence vector:** add the dequantized rows sequentially in f32, in id order. Divide by n, then L2-normalize. If there are no ids, or the norm is zero, the result is the zero vector.

## Hand features (`feature_spec = 1`)

Evaluated on `tail` unless noted. All regexes are case-insensitive and use syntax compatible with both Rust `regex` and Python `re`. A boolean feature is 1.0 or 0.0.

| # | Name | Definition |
|---|---|---|
| f0 | `last_line_question` | Take the last line of `tail` (split as Rust `str::lines`) whose `trim()` is non-empty. Trim trailing whitespace and `` *_`)"' `` characters from its end. The feature is 1 if it then ends with `?` |
| f1 | `has_question` | `tail` contains `?` |
| f2 | `offer` | `\b(let me know if|would you like me to|want me to|if you'd like|if you would like|happy to)\b` |
| f3 | `decision` | `\b(should i|which (option|one|approach)|do you want|would you prefer|please (confirm|choose|decide)|before i proceed|your call|need your (input|decision))\b` |
| f4 | `blocker` | `\b(permission denied|missing|not found|failed to|unable to|can't continue|cannot continue|blocked|need you to|credentials?|api key|unauthori[sz]ed|forbidden|rate limit)\b` |
| f5 | `completion` | `(all tests pass|\bdone\b|\bcompleted?\b|\bfinished\b|✅|successfully|\bmerged\b|\bfixed\b)` |
| f6 | `code_ratio` | Split `text` on ```` ``` ````. Sum the lengths of the odd-indexed segments (fence markers excluded; an unclosed fence runs to the end) and divide by `len(text)`. 0 if `text` is empty |
| f7 | `length` | `ln(1 + len(text)) / ln(1 + 16384)` |
| f8 | `options_list` | `tail` has at least 2 lines matching `^\s*(\d+[.)]|[-*•]|\(?[a-d][.)])\s` **and** f1 |
| f9 | `error_marker` | `(\berror\b|exception|traceback|exit code [1-9])` |
| f10 | `question_count` | `min(count('?' in tail), 5) / 5` |
| f11 | `imperative_to_user` | `\b(please (run|provide|set|add|check)|you'll need to|you will need to|you need to)\b` |

`len` counts Unicode scalar values (Python `len(str)`).

## Model input and head

- **variant `embed`:** `x = [emb(tail) (256), emb(text) (256), f0..f11 (12)]`, so 524 dimensions.
- **variant `features`:** `x = [f0..f11]`, so 12 dimensions. This is the fallback (option D).
- **Head:** `logits = W·x + b`, with `W` of shape [4][dim]. Then `p = softmax(logits)`, computed in f32 with max-subtraction.
- **Decision with `lean = alert`:** if `p[0] + p[1] ≥ lean_threshold`, choose whichever of 0 and 1 has the higher probability. Otherwise take `argmax(p)`. With `lean = balanced`, always take `argmax(p)`.
- **`lean_threshold`** is read from `head.json`; the Rust runtime rejects a head without it. The shipped head uses **0.35**, the value `model/train.py` picks under the grouped-CV tap-recall gate in [model.md](model.md#release-bar). 0.25 is only the fallback that `model/train.py` uses when no threshold is passed, and the value in the example below and in test heads.

`assets/head.json`:
```json
{"version":1,"feature_spec":1,"variant":"embed","dim":524,
 "classes":["needs_decision","blocked","done","info"],
 "W":[[...],[...],[...],[...]],"b":[0,0,0,0],"lean_threshold":0.25,
 "model_rev":"bf8b056651a2c21b8d2565580b8569da283cab23",
 "trained_on":{"synthetic":0,"real":0},"cv_macro_f1":0.0}
```

## Parity fixtures

`tests/fixtures/parity.json` is a list of entries like this:
```json
{"text":"...","ids_tail":[...],"ids_text":[...],"features":[12 floats],"probs":[4 floats],"label":"needs_decision"}
```

The Rust tests require:
- the ids to match exactly
- feature cosine > 0.9999 against a recomputation
- `|Δprob| < 1e-3`
- the same label on 100% of fixtures

## Summary extraction (not model-dependent)

Candidates are scanned from the end of `text`, and the first one found is used:

1. The last line that contains `?`.
2. The last line that matches f4 or f9.
3. The last non-empty line.

The chosen line has markdown markers removed (`#`, `*`, `_`, `` ` ``, `>`, list bullets), then goes through `sanitize`:

- Strip control and bidi characters (U+202A–U+202E, U+2066–U+2069, U+200E/F).
- Apply these redactions in this order. Each is a leftmost, non-overlapping replace over the output of the previous one. They are written as regexes here but implemented as hand-written linear scanners, so no regex is compiled at runtime. `\w` is a Unicode word character.
  1. **URLs**, as `<url>`:
     - `(?i)(https?|file|ftp|ssh)://\S+`
     - scp-style git remotes: `(?i)(?<!\w)git@[\w.\-]+:\S+`
  2. **Secret values**, as `<redacted>`, keeping the key and separator: ``(?i)(?<![A-Za-z0-9])KEY["'`]? *[=:] *["'`]?([^\s"'`]+)``, replacing only the value (group 1). An opening quote is kept and the value stops at the next whitespace or quote. `KEY` is one of `password`, `passwd`, `pass`, `pwd`, `secret`, `token`, `api_key`, `apikey`, `api-key`, `access_key`, `auth`, `bearer`, `private_key`. A `_` or `-` before the key still counts, so `DB_PASSWORD=x` is caught. Longer words such as `passwords:` and `author:` are not keys.
  3. **Emails**, as `<email>`: `[\w.+\-]+@[\w\-]+(?:\.[\w\-]+)+`.
  4. **Absolute paths**, as `<path>`:
     - Unix paths with at least two segments: `(^|[^\w/])(~?/[\w.\-]+(?:/[\w.\-]+)+)`, replaced by `$1<path>`. A path may start at the beginning of the text or after any character that is neither a word character nor `/`, such as whitespace, a quote, `(`, `[`, `=`, `:` or `,`. That boundary keeps `and/or`, `3/4` and `a/b` unchanged.
     - Windows paths: `[A-Za-z]:\\\S+`.
  5. **Key-like tokens**, as `<redacted>`: `(sk|pk|ghp|gho|ghs|github_pat|xox[abpr]|AKIA)[\w\-]{8,}`, plus any token of 32 or more characters from `[A-Za-z0-9+/=_\-]`.
- Collapse whitespace.
- Truncate to 80 scalar values, using `…` when cut.

## Hook input (fields we read; everything else is ignored)

| Event | Fields |
|---|---|
| all | `hook_event_name`, `session_id`, `cwd` |
| `Stop` | `last_assistant_message`, `background_tasks[].status` |
| `StopFailure` | (common fields only) |
| `UserPromptSubmit` | (common fields only; the prompt text is **never** read) |
| `PreToolUse` | `tool_name`, `tool_use_id`, full `tool_input` (hashed for binding; for AskUserQuestion, `questions[0].question` becomes the summary) |
| `PostToolUse`, `PostToolUseFailure` | `tool_name`, `tool_use_id` |
| `PermissionRequest` | `tool_name`, full `tool_input` (hashed for binding) |
| `PermissionDenied` | `tool_name` |
| `Notification` | `notification_type`, `message` |
| `ElicitationResult` | (common fields only) |
| `SessionStart` | `source` |
| `SessionEnd` | `reason` |

**`input_hash`:** the first 16 hex digits of the sha256 of `tool_input`, re-serialized as compact JSON with object keys sorted recursively. Python uses `json.dumps(v, sort_keys=True, separators=(",",":"), ensure_ascii=False)`, and Rust uses an equivalent canonical writer.

**Limits:** stdin is read fully, because hooks must drain it, up to 64 MiB. Large `Write` and `Edit` payloads are normal. If the input is larger than that or malformed, log `kind=bad_input` and exit 0.

## State schema

The state schema is `SessionState` in `src/contract.rs` (`STATE_VERSION = 1`). Its fields:

- `pending_tools[{tool_use_id, tool_name, input_hash}]`
- `claude{pid, start}`
- `episode{id, status: open|sending|notified|sent_uncertain|failed|closed, category, summary, trigger, tool_use_id, opened_at, bound_at, due_at, confirmed_at, attempts, retry_at, sending_since, postponed_s, closed_reason}`

**Write guard:** the sweeper writes `notified`, `failed`, or `sent_uncertain` only if the episode still has the same `id` and is still `sending` with the same `attempts`. A hook that closes the episode while a send is in flight always wins.

**`bound_at`:** the time of the most recent binding to a tool. The permission running-tool check (plan R4) counts only processes started after `bound_at`.

**macOS limitation:** process start times have 1 s resolution (`ps -o lstart`).
