"""Deterministic templated examples (tagged with their own `tpl-*` groups).

Templated rows are capped at 30% of the synthetic set. Each row combines a
work-summary body with a class-specific ending, so the model sees the same
body with different endings and must learn from the ending.

    python model/data/gen_templated.py   # rewrites model/data/synthetic/tpl-*.jsonl
"""

import json
import random
from pathlib import Path

OUT = Path(__file__).resolve().parent / "synthetic"
PER_CLASS_PER_GROUP = 17  # 3 groups x 4 classes x 17 = 204 rows

TASKS = [
    ("the pagination bug in the orders API", ["`api/orders.py`", "`tests/test_orders.py`"]),
    ("the Helm chart for the worker", ["`charts/worker/values.yaml`", "`charts/worker/templates/hpa.yaml`"]),
    ("the React settings page", ["`src/pages/Settings.tsx`", "`src/hooks/useSettings.ts`"]),
    ("the Rust config loader", ["`src/config.rs`", "`tests/config.rs`"]),
    ("the nightly backup job", ["`ops/backup.sh`", "`ops/cron.d/backup`"]),
    ("the GitHub Actions release workflow", ["`.github/workflows/release.yml`"]),
    ("the user import CSV parser", ["`importer/csv.py`", "`importer/schema.py`"]),
    ("the payments webhook handler", ["`services/payments/webhook.go`", "`services/payments/webhook_test.go`"]),
    ("the search index migration", ["`migrations/0051_search_index.sql`"]),
    ("the Terraform VPC module", ["`infra/modules/vpc/main.tf`", "`infra/modules/vpc/outputs.tf`"]),
    ("the Kafka consumer retry logic", ["`consumer/retry.kt`", "`consumer/RetryTest.kt`"]),
    ("the Django admin filters", ["`app/admin.py`", "`app/filters.py`"]),
]

VERBS = ["Updated", "Refactored", "Rewrote", "Cleaned up", "Reworked", "Patched"]
DETAILS = [
    "Removed the duplicated validation logic.",
    "Added error handling for the empty-input case.",
    "Extracted the shared helpers into one module.",
    "Replaced the hard-coded timeout with a config value.",
    "Added logging around the retry path.",
    "Renamed a few confusing variables.",
    "Added type annotations to the public functions.",
    "Wrote tests for the edge cases mentioned in the ticket.",
]

ENDINGS = {
    "needs_decision": [
        "Should I also apply this to the {other}, or keep the change scoped to {task}?",
        "Before I merge: do you want this behind a feature flag, or enabled for everyone?",
        "One open question: keep backwards compatibility with the old format, or drop it now?",
        "Which do you prefer for the default: strict mode or lenient mode?",
        "Please confirm the target branch: `main` or `release/next`?",
        "I need your decision on the naming: `{name}` or `{name}_v2`?",
        "Do you want me to squash the commits or keep the history as is?",
        "Would you prefer the retries capped at 3 or at 5?",
        "Is it OK to delete the old implementation now, or should it stay for one release?",
    ],
    "blocked": [
        "I can't run the tests, though: the test database isn't reachable (connection refused).",
        "However, pushing fails with `Permission denied (publickey)`; I need you to add a deploy key.",
        "The CI job can't start because the `{secret}` secret is missing from the repository settings.",
        "Verification is blocked: the staging API returns 401 with the credentials available here.",
        "I'm unable to finish: the package registry rejects the install with 403 Forbidden.",
        "The last step needs an API key I don't have, so I've stopped here.",
        "The build fails with `failed to fetch` for a private dependency, and this machine has no access to it.",
        "I can't continue until someone with admin rights grants the deploy role.",
    ],
    "done": [
        "All tests pass and the change is merged.",
        "Done. CI is green.",
        "✅ Lint, typecheck and tests pass.",
        "The fix is deployed to staging and verified.",
        "Everything is committed and pushed to `{branch}`.",
        "Completed; the full suite ran successfully ({n} tests).",
        "That's finished, and the ticket is closed.",
        "Merged after review; nothing else is pending.",
        "All tests pass. The remaining warnings come from a dependency and are harmless.",
    ],
    "info": [
        "Done. Want me to also update the {other}?",
        "Let me know if you'd like the same change in the {other}.",
        "If you'd like, I can add a benchmark for this path too.",
        "Happy to write docs for this if that would help.",
        "Tests pass. Would you like me to open a PR?",
        "Would it help if I added a short note about this to the changelog?",
        "Want me to run the benchmarks again to compare?",
        "Pushed. I can also clean up the unused imports if you want me to.",
    ],
}

OTHERS = ["admin dashboard", "mobile app", "CLI", "billing service", "docs", "staging config"]
SECRETS = ["DEPLOY_TOKEN", "REGISTRY_PASSWORD", "SIGNING_KEY", "SLACK_WEBHOOK"]
BRANCHES = ["fix/pagination", "feat/settings", "chore/cleanup", "fix/retry"]


def body(rng: random.Random, style: str, task: str, files: list[str]) -> str:
    verb = rng.choice(VERBS)
    details = rng.sample(DETAILS, rng.randint(1, 3))
    if style == "list":
        lines = [f"{verb} {task}:", ""] + [f"- {d}" for d in details]
        lines += [f"- Touched {', '.join(files)}"]
        return "\n".join(lines)
    if style == "table":
        rows = "\n".join(f"| {f} | modified |" for f in files)
        return f"{verb} {task}.\n\n| File | Change |\n|---|---|\n{rows}\n\n" + " ".join(details)
    return f"{verb} {task}. " + " ".join(details)


def main() -> None:
    styles = {"tpl-01-list": "list", "tpl-02-table": "table", "tpl-03-prose": "prose"}
    total = 0
    for seed, (group, style) in enumerate(styles.items()):
        rng = random.Random(1000 + seed)
        rows = []
        for label, endings in ENDINGS.items():
            for _ in range(PER_CLASS_PER_GROUP):
                task, files = rng.choice(TASKS)
                ending = rng.choice(endings).format(
                    other=rng.choice(OTHERS),
                    task=task,
                    name=rng.choice(["retry_policy", "sync_mode", "export_format"]),
                    secret=rng.choice(SECRETS),
                    branch=rng.choice(BRANCHES),
                    n=rng.randint(40, 900),
                )
                text = body(rng, style, task, files) + "\n\n" + ending
                rows.append({"text": text, "label": label, "group": group, "source": "synthetic"})
        rng.shuffle(rows)
        with open(OUT / f"{group}.jsonl", "w", encoding="utf-8") as fh:
            for r in rows:
                fh.write(json.dumps(r, ensure_ascii=False) + "\n")
        total += len(rows)
    print(f"wrote {total} templated rows")


if __name__ == "__main__":
    main()
