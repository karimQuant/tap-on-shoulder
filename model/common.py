"""Single Python implementation of docs/design/contract.md (feature_spec = 1).

Everything the Rust runtime must reproduce lives here: text preparation,
tokenization, int8 embeddings, hand features f0-f11, the head input layout,
softmax and the decision rule. All math is float32.
"""

from __future__ import annotations

import json
import math
import re
import struct
from functools import lru_cache
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
MODEL_DIR = ASSETS / "potion-base-8M"

FEATURE_SPEC = 1
MODEL_REV = "bf8b056651a2c21b8d2565580b8569da283cab23"
CLASSES = ["needs_decision", "blocked", "done", "info"]
CLASS_INDEX = {c: i for i, c in enumerate(CLASSES)}
TEXT_MAX_CHARS = 16_384
TAIL_CHARS = 600
MAX_TOKEN_IDS = 512
EMBED_DIM = 256
N_FEATURES = 12
DIMS = {"embed": 2 * EMBED_DIM + N_FEATURES, "features": N_FEATURES}
LEAN_THRESHOLD = np.float32(0.25)
# potion-base-8M remaps the vocab: [PAD]=0 [UNK]=1 [CLS]=2 [SEP]=3 [MASK]=4.
# (contract.md step 6 lists the original BERT ids 0/100-103; these are the real ones.)
SPECIAL_IDS = frozenset({0, 1, 2, 3, 4})

FEATURE_NAMES = [
    "last_line_question",
    "has_question",
    "offer",
    "decision",
    "blocker",
    "completion",
    "code_ratio",
    "length",
    "options_list",
    "error_marker",
    "question_count",
    "imperative_to_user",
]

# Rust `char::is_whitespace` (Unicode White_Space). Python's str.isspace() differs
# (it also accepts U+001C..U+001F), so `tail` uses this explicit set.
_RUST_WS = frozenset(
    "\u0009\u000a\u000b\u000c\u000d \u0085  "
    "           "
    "    　"
)
_F0_TRAIL = frozenset("*_`)\"'")

_I = re.IGNORECASE
RE_OFFER = re.compile(
    r"\b(let me know if|would you like me to|want me to|if you'd like|if you would like|happy to)\b", _I
)
RE_DECISION = re.compile(
    r"\b(should i|which (option|one|approach)|do you want|would you prefer|"
    r"please (confirm|choose|decide)|before i proceed|your call|need your (input|decision))\b",
    _I,
)
RE_BLOCKER = re.compile(
    r"\b(permission denied|missing|not found|failed to|unable to|can't continue|cannot continue|"
    r"blocked|need you to|credentials?|api key|unauthori[sz]ed|forbidden|rate limit)\b",
    _I,
)
RE_COMPLETION = re.compile(
    r"(all tests pass|\bdone\b|\bcompleted?\b|\bfinished\b|✅|successfully|\bmerged\b|\bfixed\b)", _I
)
RE_OPTION_LINE = re.compile(r"^\s*(\d+[.)]|[-*•]|\(?[a-d][.)])\s", _I)
RE_ERROR = re.compile(r"(\berror\b|exception|traceback|exit code [1-9])", _I)
RE_IMPERATIVE = re.compile(
    r"\b(please (run|provide|set|add|check)|you'll need to|you will need to|you need to)\b", _I
)


# ---------------------------------------------------------------- text prep


def rust_trim_end(s: str) -> str:
    i = len(s)
    while i > 0 and s[i - 1] in _RUST_WS:
        i -= 1
    return s[:i]


def rust_trim(s: str) -> str:
    s = rust_trim_end(s)
    i = 0
    while i < len(s) and s[i] in _RUST_WS:
        i += 1
    return s[i:]


def prepare(message: str) -> tuple[str, str]:
    """Return (text, tail) as defined by the contract."""
    text = message[:TEXT_MAX_CHARS]
    tail = rust_trim_end(text)[-TAIL_CHARS:]
    return text, tail


def rust_lines(s: str) -> list[str]:
    """Rust `str::lines`: split on '\\n', strip one trailing '\\r', no final empty line."""
    if not s:
        return []
    parts = s.split("\n")
    if parts[-1] == "":
        parts.pop()
    return [p[:-1] if p.endswith("\r") else p for p in parts]


# ---------------------------------------------------------------- tokenizer


@lru_cache(maxsize=1)
def tokenizer():
    from tokenizers import Tokenizer

    tok = Tokenizer.from_file(str(MODEL_DIR / "tokenizer.json"))
    tok.no_truncation()
    tok.no_padding()
    return tok


def token_ids(s: str) -> list[int]:
    if not s:
        return []
    ids = tokenizer().encode(s, add_special_tokens=False).ids
    return [i for i in ids if i not in SPECIAL_IDS][:MAX_TOKEN_IDS]


# ---------------------------------------------------------------- embeddings


def _load_safetensors(path: Path, name: str) -> np.ndarray:
    raw = path.read_bytes()
    (hlen,) = struct.unpack("<Q", raw[:8])
    header = json.loads(raw[8 : 8 + hlen])
    meta = header[name]
    if meta["dtype"] != "F32":
        raise ValueError(f"unexpected dtype {meta['dtype']}")
    start, end = meta["data_offsets"]
    base = 8 + hlen
    arr = np.frombuffer(raw[base + start : base + end], dtype="<f4")
    return arr.reshape(meta["shape"]).astype(np.float32)


def quantize_int8(m: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Per-row int8: scale = max|row|/127 (1 if 0), q = clamp(round_half_away(x/scale))."""
    m = m.astype(np.float32)
    mx = np.max(np.abs(m), axis=1).astype(np.float32)
    scale = (mx / np.float32(127.0)).astype(np.float32)
    scale[mx == 0] = np.float32(1.0)
    v = (m / scale[:, None]).astype(np.float32)
    q = np.sign(v) * np.floor(np.abs(v) + np.float32(0.5))
    q = np.clip(q, -127, 127).astype(np.int8)
    return q, scale


@lru_cache(maxsize=1)
def embedding_table() -> np.ndarray:
    m = _load_safetensors(MODEL_DIR / "model.safetensors", "embeddings")
    q, scale = quantize_int8(m)
    return (q.astype(np.float32) * scale[:, None]).astype(np.float32)


def embed_ids(ids: list[int]) -> np.ndarray:
    if not ids:
        return np.zeros(EMBED_DIM, dtype=np.float32)
    table = embedding_table()
    acc = np.zeros(EMBED_DIM, dtype=np.float32)
    for i in ids:  # sequential f32 sum, the same order Rust uses
        acc += table[i]
    mean = (acc / np.float32(len(ids))).astype(np.float32)
    norm = np.float32(math.sqrt(float(np.dot(mean, mean))))
    if norm == 0:
        return np.zeros(EMBED_DIM, dtype=np.float32)
    return (mean / norm).astype(np.float32)


# ---------------------------------------------------------------- features


def _f0_last_line_question(tail: str) -> bool:
    for line in reversed(rust_lines(tail)):
        if not rust_trim(line):
            continue
        end = len(line)
        while end > 0 and (line[end - 1] in _RUST_WS or line[end - 1] in _F0_TRAIL):
            end -= 1
        return line[:end].endswith("?")
    return False


def code_ratio(text: str) -> float:
    """Characters between ``` fences (markers excluded) / len(text).

    Segments at odd positions of text.split("```") are inside a fence; an
    unclosed final fence runs to the end of the text.
    """
    if not text:
        return 0.0
    parts = text.split("```")
    inside = sum(len(p) for p in parts[1::2])
    return inside / len(text)


def features(message: str) -> np.ndarray:
    text, tail = prepare(message)
    f = np.zeros(N_FEATURES, dtype=np.float32)
    has_q = "?" in tail
    f[0] = _f0_last_line_question(tail)
    f[1] = has_q
    f[2] = RE_OFFER.search(tail) is not None
    f[3] = RE_DECISION.search(tail) is not None
    f[4] = RE_BLOCKER.search(tail) is not None
    f[5] = RE_COMPLETION.search(tail) is not None
    f[6] = code_ratio(text)
    f[7] = math.log(1 + len(text)) / math.log(1 + TEXT_MAX_CHARS)
    opt_lines = sum(1 for line in rust_lines(tail) if RE_OPTION_LINE.search(line))
    f[8] = opt_lines >= 2 and has_q
    f[9] = RE_ERROR.search(tail) is not None
    f[10] = min(tail.count("?"), 5) / 5
    f[11] = RE_IMPERATIVE.search(tail) is not None
    return f


# ---------------------------------------------------------------- model input


def ids_for(message: str) -> tuple[list[int], list[int]]:
    text, tail = prepare(message)
    return token_ids(tail), token_ids(text)


def build_x(message: str, variant: str) -> np.ndarray:
    feats = features(message)
    if variant == "features":
        return feats
    if variant != "embed":
        raise ValueError(f"unknown variant {variant!r}")
    ids_tail, ids_text = ids_for(message)
    return np.concatenate([embed_ids(ids_tail), embed_ids(ids_text), feats]).astype(np.float32)


def softmax(logits: np.ndarray) -> np.ndarray:
    z = logits.astype(np.float32)
    z = z - np.max(z)
    e = np.exp(z).astype(np.float32)
    return (e / np.sum(e, dtype=np.float32)).astype(np.float32)


def decide(p: np.ndarray, lean: str = "alert", threshold: float = LEAN_THRESHOLD) -> int:
    if lean == "alert" and np.float32(p[0] + p[1]) >= np.float32(threshold):
        return 0 if p[0] >= p[1] else 1
    return int(np.argmax(p))


# ---------------------------------------------------------------- head io


def load_head(path: Path) -> dict:
    head = json.loads(Path(path).read_text())
    head["W_np"] = np.asarray(head["W"], dtype=np.float32)
    head["b_np"] = np.asarray(head["b"], dtype=np.float32)
    return head


def head_probs(head: dict, x: np.ndarray) -> np.ndarray:
    logits = (head["W_np"] @ x.astype(np.float32) + head["b_np"]).astype(np.float32)
    return softmax(logits)


def load_jsonl(path: Path) -> list[dict]:
    out = []
    with open(path, encoding="utf-8") as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            if row.get("label") not in CLASS_INDEX:
                raise ValueError(f"{path}:{n}: bad label {row.get('label')!r}")
            if not isinstance(row.get("text"), str):
                raise ValueError(f"{path}:{n}: missing text")
            out.append(row)
    return out
