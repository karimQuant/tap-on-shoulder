# How it works

> [!NOTE]
> This page describes how v0.1 works. The Claude Code behaviour it relies on is listed, with links and spike results, in [Verified interfaces](design/verified-interfaces.md).

![tap-on-shoulder workflow](assets/workflow.svg)

<sub>Source: [`assets/workflow.d2`](assets/workflow.d2). Re-render with `d2 --layout elk workflow.d2 workflow.svg`.</sub>

## The idea in one sentence

A **waiting episode** opens when a session stops and needs you. If it is still open after the grace period, you get **exactly one** notification for it.

## 1. Hooks record events

Claude Code runs the plugin's hooks on session events.

| Event | Opens or re-arms | Closes |
|---|---|---|
| `Stop`: Claude finished its turn | An episode with the classified category (see below) | Any episode bound to a tool call |
| `StopFailure`: the turn ended with an API error | A `blocked` episode | Any episode bound to a tool call |
| `PermissionRequest` | A `needs_permission` episode bound to the requested call (`needs_decision` for `AskUserQuestion` / `ExitPlanMode`) | |
| `PreToolUse` | A `needs_decision` episode bound to the call, for `AskUserQuestion` and `ExitPlanMode` only | An unbound episode (work resumed) |
| `PostToolUse` / `PostToolUseFailure` | | The episode bound to this call (approved and finished), and any unbound episode |
| `PermissionDenied` | | The episode bound to the denied tool, and any unbound episode |
| `Notification` | `elicitation_dialog`: a `needs_decision` episode. `permission_prompt` only confirms that an open permission episode's prompt is on screen | |
| `ElicitationResult` | | The elicitation episode |
| `UserPromptSubmit`: you replied | | Any episode |
| `SessionStart` / `SessionEnd` | | Any episode (`SessionEnd` deletes the session's state) |

A permission episode stays open while the prompt is on screen: approving it only lets the tool start, so the episode closes on the `PostToolUse` (or `PostToolUseFailure`) of the bound call, or when you reply. If several pending calls of the same tool could be behind the prompt, it closes once none of them is still pending.

If a `Stop` arrives while background tasks are still running and the message is classified `done` or `info`, it doesn't open an episode: the session is still **working**, and a new `Stop` arrives when the work finishes. A question or a blocker still opens one.

## 2. One sweeper for all sessions

When an episode opens, the hook writes a small state file and makes sure a single background process, `tap sweep`, is running. It is not a daemon: it exits as soon as nothing is waiting. Every few seconds it checks each open episode against four conditions:

- Is the grace period over?
- Is the episode still open? Any reply or new activity closes it, and a newer turn replaces it.
- Is the Claude Code process for that session still running?
- For a permission prompt: is it still unanswered? If the approved tool is already running, the tap is postponed.

It sends only when all four hold, and it sends **at most once** per episode.

## 3. Local classification

| State | Example | Tap by default? |
|---|---|---|
| `working` | Long build, background task still running | Never |
| `needs_decision` | "Which migration strategy do you want?" | Yes |
| `needs_permission` | Permission prompt waiting for approval | Yes |
| `blocked` | API error, missing credentials | Yes |
| `done` | "All tests pass, PR ready." | Only with `alerts.completion` |
| `info` | "Let me know if you'd like anything else." | No |
| `unknown` | Missing or contradictory events | Logged and shown in `tap doctor`. Never treated as idle |

- **Event rules decide first.** A permission prompt means `needs_permission`. An API error means `blocked`. Claude asking through its question tool means `needs_decision`.
- **The embedded model handles the rest.** When the rules can't decide, the plugin reads Claude's final message with a small model (model2vec static embeddings plus a linear head, about 8 MB, built into the binary). The shipped head is a *preview* trained on synthetic data; see [model.md](design/model.md). It runs on your CPU in milliseconds.
- **The model only labels.** It is not generative, so all it can do is pick one of the labels above.
- **The summary is copied, not written.** It is the last question or error line from the message, redacted and cut to 80 characters.

Classification runs in the `Stop` hook, as soon as the episode opens, because that is when its grace period gets chosen. A `done` or `info` result with its alert turned off closes the episode right there (`suppressed`), so the sweeper never sees it. Questions, permissions, and blocked states each have their own grace period.

## 4. One notification

The plugin sends one notification to your ntfy topic: project folder name, wait time, category, and the summary. It claims the episode (`sending`) under a lock before sending and then records `notified`, `failed`, or `sent_uncertain`, so a restart can't send it again.

## Sessions, subagents, background work

- **Sessions:** each Claude Code session is tracked by its `session_id`, so concurrent sessions alert independently.
- **Subagents and background tasks:** these count as the parent session still **working**. They don't trigger alerts of their own.

## When something fails

| Failure | Behaviour |
|---|---|
| Binary missing or unverified | A one-line warning at session start, plus a log entry. No taps until the background install succeeds |
| Embedded model fails to load | Rules-only classification; `tap doctor` shows `model: rules` |
| Classifier unsure | `classifier.lean = "alert"`: if `p(needs_decision) + p(blocked)` ≥ the head's `lean_threshold` (0.35), tap rather than stay silent |
| ntfy unreachable | Up to 3 attempts, then recorded as `failed`, logged, and shown in `tap doctor` (which exits 1 while the server is unreachable). Never counted as delivered |
| Machine slept or restarted | Due times are wall-clock, and the next hook restarts the sweeper. The episode status (`sending`, `notified`) prevents duplicates |
