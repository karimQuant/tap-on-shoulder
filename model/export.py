"""Export the trained heads and the Rust parity fixtures.

    python model/export.py

Reads model/artifacts/heads.pkl (from train.py) and writes:
- assets/head.json                     variant `embed`, dim 524 (shipped)
- model/artifacts/head_features.json   variant `features`, dim 12 (fallback, option D)
- tests/fixtures/parity.json           fixtures computed through common.py with the
                                       *exported* (rounded) embed head
"""

from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

ROOT = common.ROOT
ARTIFACTS = Path(__file__).resolve().parent / "artifacts"
HEAD_PATH = ROOT / "assets" / "head.json"
FEATURES_HEAD_PATH = ARTIFACTS / "head_features.json"
PARITY_PATH = ROOT / "tests" / "fixtures" / "parity.json"

SIG_DIGITS = 7  # float32 has ~7 significant digits; keeps head.json small
HEAD_MAX_BYTES = 120 * 1024
PARITY_MAX_BYTES = 400 * 1024
MIN_FIXTURES = 60
MIN_NEAR = 10
NEAR_BAND = 0.15  # |p0 + p1 - lean_threshold| <= NEAR_BAND counts as near-threshold
# Fixtures closer than this to a decision boundary are skipped: f32 summation order
# differences between Python and Rust must never be able to flip a label.
MIN_MARGIN = 0.01


def fmt(v: float) -> float:
    return float(f"{float(np.float32(v)):.{SIG_DIGITS}g}")


def head_dict(entry: dict, variant: str, trained_on: dict) -> dict:
    model = entry["model"]
    w = model.coef_.astype(np.float32)
    b = model.intercept_.astype(np.float32)
    assert list(model.classes_) == list(range(len(common.CLASSES))), model.classes_
    assert w.shape == (4, common.DIMS[variant]), w.shape
    return {
        "version": 1,
        "feature_spec": common.FEATURE_SPEC,
        "variant": variant,
        "dim": int(w.shape[1]),
        "classes": common.CLASSES,
        "W": [[fmt(v) for v in row] for row in w],
        "b": [fmt(v) for v in b],
        "lean_threshold": float(entry["lean_threshold"]),
        "model_rev": common.MODEL_REV,
        "trained_on": trained_on,
        "cv_macro_f1": round(entry["cv"]["macro_f1"], 4),
        "cv_tap_recall": round(entry["cv"]["tap_recall"], 4),
        "cv_tap_precision": round(entry["cv"]["tap_precision"], 4),
    }


def write_head(path: Path, head: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    rows = ",\n  ".join(json.dumps(r, separators=(",", ":")) for r in head["W"])
    meta = {k: v for k, v in head.items() if k != "W"}
    body = json.dumps(meta, ensure_ascii=False, separators=(",", ":"))
    # Put W on its own lines for readable diffs; still valid JSON.
    path.write_text(body[:-1] + ',\n "W":[\n  ' + rows + "\n ]}\n", encoding="utf-8")


def margins(p: np.ndarray, thr: float) -> float:
    """Distance of p from any boundary of the lean=alert decision rule."""
    s = float(p[0] + p[1])
    m = abs(s - thr)
    if s >= thr:
        m = min(m, abs(float(p[0] - p[1])))
    else:
        top = np.sort(p)[::-1]
        m = min(m, float(top[0] - top[1]))
    return m


def fixture(head: dict, text: str) -> dict | None:
    ids_tail, ids_text = common.ids_for(text)
    feats = common.features(text)
    x = common.build_x(text, "embed")
    p = common.head_probs(head, x)
    if margins(p, head["lean_threshold"]) < MIN_MARGIN:
        return None
    return {
        "text": text,
        "ids_tail": ids_tail,
        "ids_text": ids_text,
        "features": [float(f"{float(v):.7g}") for v in feats],
        "probs": [float(f"{float(v):.7g}") for v in p],
        "label": common.CLASSES[common.decide(p, "alert", head["lean_threshold"])],
    }


EDGE_CASES = [
    "",
    "   \n\t  ",
    "?",
    "数据库迁移已经完成了吗？请确认是否可以部署到生产环境。",
    "データベースのバックアップは毎日にしますか、それとも毎時にしますか？",
    "Café, naïve façade, résumé, Ångström, Øresund, Ελληνικά, Привет — accents and scripts.",
    "Deployed 🚀🎉✅ — all green! 👍🏽 Want me to also tag the release? 🤔",
    "Zero​width and ‮RTL override‬ and soft­hyphen and nbsp here. Done.",
    "Tabs\tand\r\nCRLF\r\nline endings\r\nShould I convert them?\r\n",
    "supercalifragilisticexpialidocious" * 4 + " is one very long word. Done.",
    "x" * 101 + " word longer than one hundred characters.",
    "```python\n" + "\n".join(f"def f{i}(x):\n    return x * {i}  # why? because" for i in range(120)) + "\n```\nAll tests pass.",
    "Unclosed fence:\n```bash\nrm -rf /path/to/build\nmake all\n",
    "1. Option A\n2. Option B\n(c) Option C\n- bullet\nWhich one?",
    "Error: exit code 2\nTraceback (most recent call last):\n  File \"x.py\"\nException: boom",
]


def long_text() -> str:
    block = (
        "I went through the service logs and the deploy history for the last week. "
        "Most of the noise is from the retry worker and is expected.\n"
        "```\n2026-10-20T09:14:02Z INFO  sync run ok records=1200\n```\n"
    )
    body = block * (20_000 // len(block) + 1)
    return body[:20_000] + "\n\nShould I roll back the worker to the previous version, or keep debugging in place?"


def main() -> int:
    with open(ARTIFACTS / "heads.pkl", "rb") as fh:
        heads = pickle.load(fh)

    embed = head_dict(heads["embed"], "embed", heads["trained_on"])
    feats = head_dict(heads["features"], "features", heads["trained_on"])
    write_head(HEAD_PATH, embed)
    write_head(FEATURES_HEAD_PATH, feats)
    size = HEAD_PATH.stat().st_size
    print(
        f"wrote {HEAD_PATH.relative_to(ROOT)} ({size} bytes), lean_threshold={embed['lean_threshold']}, "
        f"cv_macro_f1={embed['cv_macro_f1']}, cv_tap_recall={embed['cv_tap_recall']}"
    )
    print(f"wrote {FEATURES_HEAD_PATH.relative_to(ROOT)}, cv_macro_f1={feats['cv_macro_f1']}")
    if size > HEAD_MAX_BYTES:
        print(f"FAIL: head.json is {size} bytes > {HEAD_MAX_BYTES}")
        return 1

    head = common.load_head(HEAD_PATH)  # fixtures use the rounded, exported weights
    rows = []
    for p in sorted((ROOT / "model" / "data" / "synthetic").glob("*.jsonl")):
        rows += common.load_jsonl(p)
    rng = np.random.default_rng(7)
    rng.shuffle(rows)

    fixtures, skipped = [], 0
    for text in EDGE_CASES + [long_text()]:
        fx = fixture(head, text)
        if fx is None:
            print(f"warning: edge case skipped (margin < {MIN_MARGIN}): {text[:40]!r}")
            continue
        fixtures.append(fx)

    near, regular = [], []
    for r in rows:
        if len(r["text"]) > 1500:
            continue  # keep the file small; long inputs are covered above
        fx = fixture(head, r["text"])
        if fx is None:
            skipped += 1
            continue
        s = fx["probs"][0] + fx["probs"][1]
        (near if abs(s - head["lean_threshold"]) <= NEAR_BAND else regular).append(fx)

    fixtures += near[:16]
    per_label: dict[str, int] = {}
    for fx in regular:
        if len(fixtures) >= 72:
            break
        if per_label.get(fx["label"], 0) >= 12:
            continue
        per_label[fx["label"]] = per_label.get(fx["label"], 0) + 1
        fixtures.append(fx)

    n_near = sum(abs(f["probs"][0] + f["probs"][1] - head["lean_threshold"]) <= NEAR_BAND for f in fixtures)
    PARITY_PATH.parent.mkdir(parents=True, exist_ok=True)
    PARITY_PATH.write_text(
        "[\n" + ",\n".join(json.dumps(f, ensure_ascii=False, separators=(",", ":")) for f in fixtures) + "\n]\n",
        encoding="utf-8",
    )
    psize = PARITY_PATH.stat().st_size
    labels = {c: sum(f["label"] == c for f in fixtures) for c in common.CLASSES}
    print(
        f"wrote {PARITY_PATH.relative_to(ROOT)} ({psize} bytes): {len(fixtures)} fixtures, "
        f"{n_near} near threshold, labels {labels}, {skipped} skipped for margin < {MIN_MARGIN}"
    )
    ok = len(fixtures) >= MIN_FIXTURES and n_near >= MIN_NEAR and psize <= PARITY_MAX_BYTES
    if not ok:
        print("FAIL: parity fixture requirements not met")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
