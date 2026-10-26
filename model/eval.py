"""Evaluate an exported head on a real, hand-labelled holdout against the release bar.

    python model/eval.py --holdout path/to/holdout.jsonl [--head assets/head.json] [--lean alert]

Release bar (docs/design/model.md): needs_decision recall >= 0.95 and macro-F1 >= 0.85,
measured on >= 200 real endings never used in training. Prints PASS or FAIL; exits 1 on FAIL.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
from sklearn.metrics import confusion_matrix, f1_score, recall_score

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common  # noqa: E402

BAR_ND_RECALL = 0.95
BAR_MACRO_F1 = 0.85
MIN_HOLDOUT = 200


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--holdout", type=Path, required=True)
    ap.add_argument("--head", type=Path, default=common.ROOT / "assets" / "head.json")
    ap.add_argument("--lean", choices=["alert", "balanced"], default="alert")
    args = ap.parse_args()

    head = common.load_head(args.head)
    rows = common.load_jsonl(args.holdout)
    if not rows:
        print("FAIL: holdout is empty")
        return 1
    y = np.array([common.CLASS_INDEX[r["label"]] for r in rows])
    pred = np.array(
        [
            common.decide(
                common.head_probs(head, common.build_x(r["text"], head["variant"])), args.lean, head["lean_threshold"]
            )
            for r in rows
        ]
    )
    labels = list(range(len(common.CLASSES)))
    macro = float(f1_score(y, pred, average="macro", labels=labels, zero_division=0))
    recalls = recall_score(y, pred, average=None, labels=labels, zero_division=0)
    cm = confusion_matrix(y, pred, labels=labels)

    print(f"head: {args.head} (variant {head['variant']}), lean={args.lean}, holdout rows: {len(rows)}")
    print(f"macro-F1: {macro:.4f}")
    for name, r in zip(common.CLASSES, recalls):
        print(f"  recall {name:<15} {r:.3f}")
    print("  confusion (rows = true, cols = pred):")
    for name, row in zip(common.CLASSES, cm):
        print(f"  {name:<16}" + " ".join(f"{v:>6}" for v in row))

    ok = recalls[0] >= BAR_ND_RECALL and macro >= BAR_MACRO_F1
    print(
        f"bar: needs_decision recall {recalls[0]:.3f} (>= {BAR_ND_RECALL}), "
        f"macro-F1 {macro:.3f} (>= {BAR_MACRO_F1})"
    )
    if len(rows) < MIN_HOLDOUT:
        print(f"note: holdout has {len(rows)} rows; the release bar is defined on >= {MIN_HOLDOUT}")
        ok = False
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
