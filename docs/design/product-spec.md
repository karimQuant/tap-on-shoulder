# Product spec

**Status:** draft for v0.1. Statements are **Proposed** unless marked otherwise. The Claude Code facts behind this spec are in [verified-interfaces.md](verified-interfaces.md).

## Problem

People run several long Claude Code sessions in parallel and walk away. A session that stops to ask a question sits idle until someone happens to look. Existing notifiers fire after every response (noisy) or after a fixed ~60 s (`idle_prompt`), and they don't say *what* is needed.

## Goal

Send **one** notification per **waiting episode** when a session has been **continuously** waiting on the user for longer than a configurable grace period. The notification says which project is waiting and what it needs.

## Users

- **Primary:** a developer running 2–10 concurrent Claude Code CLI sessions on one Linux or macOS machine, away from the keyboard for minutes to hours.
- **Secondary:** contributors who want to tune the classifier or add notification channels.

## Non-goals (v0.1)

- Acting on sessions in any way: answering, approving, resuming, or waking Claude.
- A dashboard or session list (Claude Code's `claude agents` already does this).
- A hosted backend, cloud LLM classification, or cross-machine supervision.
- Notification channels other than ntfy.
- Repeat or escalation reminders.
- Coding agents other than Claude Code.

## Definitions

| Term | Meaning |
|---|---|
| **Session** | One Claude Code session, identified by `session_id` |
| **Waiting episode** | Interval that starts when a session stops and needs the user, and ends when the user responds, the session resumes, or the session ends. It has a unique `episode_id` |
| **Grace period** | How long an episode must stay open before a notification is sent. It is set per category |
| **Category** | `needs_decision`, `needs_permission`, `blocked`, `done`, `info` (episode categories), plus `working` and `unknown` (session states that never open an episode) |
| **Tap** | The single notification sent for an episode |

## User stories

1. As a developer, when Claude asks me a question and I'm away, I get one tap after 5 minutes. It names the project and the question.
2. As a developer, if I answer within 5 minutes, I get nothing.
3. As a developer, long silent tool runs and background work never produce taps.
4. As a developer, I choose whether finished tasks produce a tap.
5. As a developer, I can set a different grace period for permission prompts.
6. As a developer, I can test the whole flow in seconds without waiting 5 real minutes.
7. As a privacy-conscious user, I can keep summaries out of notifications, use my own ntfy server, and know that nothing else leaves my machine.
8. As a user, when something is broken, `tap doctor` tells me what and why.

## Settings

The full reference is in [configuration.md](../configuration.md).

| Setting | Default | Notes |
|---|---|---|
| `grace.question` / `grace.permission` / `grace.blocked` | `5m` each | Minimum `5s`, maximum `24h` |
| `alerts.completion` | `false` | Tap on `done` |
| `alerts.info` | `false` | Tap on `info` |
| `classifier.mode` | `model` | `rules` disables the embedded model |
| `classifier.lean` | `alert` | When the model is uncertain, choose the category that taps |
| Project config | — | `<repo>/.claude/tap-on-shoulder.toml` may only shorten `grace.*` and set `alerts.completion` |
| `notify.ntfy.server` / `topic` / `token_env` | `https://ntfy.sh` / random / unset | **User config only.** A project config can never change these |
| `notify.ntfy.privacy` | `summary` | `category-only` drops the summary |
| `training.collect` | `false` | Opt-in. Stores redacted endings locally for labelling. See [model.md](model.md) |

## Notification policy

1. An episode opens only on the arming events listed in [architecture.md](architecture.md#event-mapping).
2. **Category grace:** a tap is due at `opened_at + grace[category]`. Waits classified as `done` or `info` use `grace.question`, and tap only if enabled.
3. **At most one tap per episode.** The `notified` flag is set under a lock *before* sending. A failed send is retried at most 3 times within 2 minutes, then recorded as failed. It is never silently counted as delivered.
4. **Cancellation:** any activity event for the session closes the episode. A newer turn produces a new `episode_id`, so a stale check can never notify about a newer turn.
5. **Concurrent sessions:** every session is independent. There is no grouping in v0.1.
6. **Unknown:** if state can't be determined (corrupt file, missing events), the session state is `unknown`. It is logged and shown in `tap doctor`, and it does **not** tap.
7. **Binary missing:** no taps are sent until the background install succeeds. `SessionStart` shows a one-line warning, and `tap doctor` explains why.

## Notification content

- **Title:** `<project> · waiting <duration>`
- **Body:** `<category label>: <summary>`. With `category-only`, the body is just the category label.
- `<project>` is the basename of `cwd`. The summary is extractive (copied from the message, never generated), redacted, and at most 80 characters.
- **Priority by category:** `needs_permission` and `needs_decision` → high, `blocked` → default, `done` and `info` → low.

## Acceptance scenarios

| # | Scenario | Expected |
|---|---|---|
| 1 | Claude stops with a question and you don't respond | Exactly one tap after `grace.question`, naming the project and question |
| 2 | You respond before the grace period ends | No tap |
| 3 | Claude resumes (new activity) before the grace period ends | The sweeper sees the episode closed or replaced and sends nothing |
| 4 | A tool runs silently for over 5 minutes | No tap (no arming event; activity events keep the session `working`) |
| 5 | A task completes successfully | Tap only if `alerts.completion = true` |
| 6 | A permission prompt appears | Tap after `grace.permission` unless it was approved or denied first |
| 7 | Two sessions wait concurrently | Two taps, each with the correct project |
| 8 | The sweeper process dies (reboot, OOM) or the plugin restarts mid-episode | No duplicate tap. On the next `SessionStart`, stale episodes are closed. A tap already sent is never sent again |
| 9a | The embedded model fails to load | Rules-only classification, logged, shown in `tap doctor` |
| 9b | ntfy is unreachable | Bounded retry, then recorded as failed and shown in `tap doctor` |
| 10 | The final message contains instructions aimed at the classifier | The output is only a category label and an extractive summary. No other effect |
| 11 | A project config tries to change `notify.*` | Ignored with a warning |
| 12 | `TAP_GRACE_OVERRIDE=5s` | The full flow completes in about 5 seconds |

## Success criteria for v0.1

- Every acceptance scenario is covered by automated tests that simulate hook JSON on stdin with time overrides.
- Hook handler overhead is ≤ 20 ms at p95 on a typical laptop, so sessions never feel slower.
- Zero network requests other than ntfy (and the release download).
- The classifier's release bar is defined in [model.md](model.md). Until it is met, the model is labelled *preview* and `lean = alert` keeps misses low.
