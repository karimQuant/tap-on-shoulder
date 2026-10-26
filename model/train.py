"""Train the logistic heads (variants `embed` and `features`) with grouped CV.

    python model/train.py --data model/data/synthetic [--extra labeled.jsonl] [--report]

`--report` is the preview gate (synthetic). On the embed variant's grouped-CV
out-of-fold probabilities, with the lean=alert rule at the chosen threshold, it
exits 1 unless tap-recall >= 0.95 and 4-class macro-F1 >= 0.85. Tap classes are
{needs_decision, blocked}; tap-recall is the fraction of true tap rows predicted
as either tap class.

lean_threshold is chosen from THRESHOLDS as the one with the best tap-precision
among those with tap-recall >= 0.95 (ties: higher macro-F1), and is written into
the head by export.py.
Fitted models are saved to model/artifacts/heads.pkl for export.py.
"""

from __future__ import annotations

import argparse
import pickle
import sys
from collections import Counter
from pathlib import Path

import numpy as np
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import confusion_matrix, f1_score, recall_score
from sklearn.model_selection import GroupKFold

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

ARTIFACTS = Path(__file__).resolve().parent / "artifacts"
C_GRID = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0]
GATE_TAP_RECALL = 0.95
GATE_MACRO_F1 = 0.85
THRESHOLDS = [0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50]
TAP = (0, 1)  # needs_decision, blocked
N_SPLITS = 5


def load(data_dir: Path, extra: list[Path]) -> list[dict]:
    rows = []
    for p in sorted(data_dir.glob("*.jsonl")):
        rows += common.load_jsonl(p)
    for p in extra:
        for r in common.load_jsonl(p):
            r.setdefault("source", "real")
            r.setdefault("group", f"extra:{p.name}")
            rows.append(r)
    return rows


def make_model(c: float) -> LogisticRegression:
    # lbfgs fits a multinomial (softmax) model for multi-class targets.
    return LogisticRegression(C=c, class_weight="balanced", max_iter=5000, solver="lbfgs")


def grouped_cv(x: np.ndarray, y: np.ndarray, groups: np.ndarray, c: float) -> np.ndarray:
    """Out-of-fold probabilities from GroupKFold (no group appears in both train and test)."""
    probs = np.zeros((len(y), len(common.CLASSES)), dtype=np.float32)
    for tr, te in GroupKFold(n_splits=N_SPLITS).split(x, y, groups):
        m = make_model(c).fit(x[tr], y[tr])
        probs[te] = m.predict_proba(x[te]).astype(np.float32)
    return probs


def decide_all(probs: np.ndarray, lean: str, threshold: float = 0.25) -> np.ndarray:
    return np.array([common.decide(p, lean, threshold) for p in probs])


def tap_stats(y: np.ndarray, pred: np.ndarray) -> dict:
    true_tap = np.isin(y, TAP)
    pred_tap = np.isin(pred, TAP)
    tp = int((true_tap & pred_tap).sum())
    return {
        "tap_recall": tp / max(int(true_tap.sum()), 1),
        "tap_precision": tp / max(int(pred_tap.sum()), 1),
    }


def choose_threshold(y: np.ndarray, probs: np.ndarray) -> tuple[float, dict]:
    table = {}
    for t in THRESHOLDS:
        pred = decide_all(probs, "alert", t)
        table[t] = {**metrics(y, pred), **tap_stats(y, pred)}
    print("\n  lean=alert threshold sweep (grouped CV):")
    print("  thr    tap_rec  tap_prec  macroF1  nd_rec")
    for t, m in table.items():
        print(
            f"  {t:.2f}   {m['tap_recall']:.3f}    {m['tap_precision']:.3f}     "
            f"{m['macro_f1']:.4f}   {m['recall'][0]:.3f}"
        )
    ok = [t for t in THRESHOLDS if table[t]["tap_recall"] >= GATE_TAP_RECALL]
    pool = ok or THRESHOLDS
    key = (lambda t: (table[t]["tap_precision"], table[t]["macro_f1"])) if ok else (
        lambda t: table[t]["tap_recall"]
    )
    best = max(pool, key=key)
    return best, table[best]


def metrics(y: np.ndarray, pred: np.ndarray) -> dict:
    labels = list(range(len(common.CLASSES)))
    return {
        "macro_f1": float(f1_score(y, pred, average="macro", labels=labels)),
        "recall": recall_score(y, pred, average=None, labels=labels, zero_division=0).tolist(),
        "confusion": confusion_matrix(y, pred, labels=labels).tolist(),
    }


def print_metrics(title: str, m: dict) -> None:
    print(f"\n== {title}")
    print(f"macro-F1: {m['macro_f1']:.4f}")
    for name, r in zip(common.CLASSES, m["recall"]):
        print(f"  recall {name:<15} {r:.3f}")
    print("  confusion (rows = true, cols = pred):")
    print("  " + " " * 16 + " ".join(f"{c[:6]:>7}" for c in common.CLASSES))
    for name, row in zip(common.CLASSES, m["confusion"]):
        print(f"  {name:<16}" + " ".join(f"{v:>7}" for v in row))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, default=Path("model/data/synthetic"))
    ap.add_argument("--extra", type=Path, action="append", default=[], help="labeled real data (JSONL)")
    ap.add_argument(
        "--report",
        action="store_true",
        help=f"exit 1 unless embed tap-recall >= {GATE_TAP_RECALL} and macro-F1 >= {GATE_MACRO_F1}",
    )
    args = ap.parse_args()

    rows = load(args.data, args.extra)
    y = np.array([common.CLASS_INDEX[r["label"]] for r in rows])
    groups = np.array([r.get("group", "none") for r in rows])
    sources = Counter("real" if r.get("source") != "synthetic" else "synthetic" for r in rows)
    print(f"rows: {len(rows)}  groups: {len(set(groups))}  sources: {dict(sources)}")
    print("per class: " + ", ".join(f"{c}={int((y == i).sum())}" for i, c in enumerate(common.CLASSES)))

    out = {"trained_on": {"synthetic": sources.get("synthetic", 0), "real": sources.get("real", 0)}}
    for variant in ("embed", "features"):
        x = np.stack([common.build_x(r["text"], variant) for r in rows]).astype(np.float32)
        scores, oof = {}, {}
        for c in C_GRID:
            oof[c] = grouped_cv(x, y, groups, c)
            scores[c] = metrics(y, decide_all(oof[c], "balanced"))
        best_c = max(C_GRID, key=lambda c: scores[c]["macro_f1"])
        print(f"\n[{variant}] C grid: " + ", ".join(f"C={c}: {scores[c]['macro_f1']:.4f}" for c in C_GRID))
        title = f"{variant} (dim {x.shape[1]}), grouped {N_SPLITS}-fold CV, C={best_c}"
        print_metrics(f"{title}, lean=balanced (argmax)", scores[best_c])
        thr, gate = choose_threshold(y, oof[best_c])
        print_metrics(f"{title}, lean=alert, threshold={thr:.2f} (shipped)", gate)
        print(
            f"  tap-recall {gate['tap_recall']:.3f}  tap-precision {gate['tap_precision']:.3f}  "
            f"needs_decision recall {gate['recall'][0]:.3f}"
        )
        model = make_model(best_c).fit(x, y)
        out[variant] = {
            "model": model,
            "C": best_c,
            "cv": gate,
            "cv_argmax": scores[best_c],
            "lean_threshold": thr,
            "dim": x.shape[1],
        }

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    with open(ARTIFACTS / "heads.pkl", "wb") as fh:
        pickle.dump(out, fh)
    print(f"\nsaved fitted heads to {ARTIFACTS.relative_to(common.ROOT)}/heads.pkl")

    g = out["embed"]["cv"]
    passed = g["tap_recall"] >= GATE_TAP_RECALL and g["macro_f1"] >= GATE_MACRO_F1
    summary = (
        f"embed preview gate @ lean_threshold={out['embed']['lean_threshold']:.2f}: "
        f"tap-recall {g['tap_recall']:.3f} (>= {GATE_TAP_RECALL}), "
        f"macro-F1 {g['macro_f1']:.4f} (>= {GATE_MACRO_F1}), "
        f"tap-precision {g['tap_precision']:.3f}, needs_decision recall {g['recall'][0]:.3f}"
    )
    if args.report:
        print(("PASS: " if passed else "FAIL: ") + summary)
        return 0 if passed else 1
    print(summary)
    return 0

if __name__ == "__main__":
    sys.exit(main())
