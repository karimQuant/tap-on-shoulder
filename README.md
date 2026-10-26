# tap-on-shoulder

**Your agents keep working. We tap you when you're needed.**

A Claude Code plugin that watches all your Claude Code sessions and sends you a single notification when one of them has been waiting on you for a while. You get one notification per wait, not one after every response.

> [!WARNING]
> **Planning stage — not implemented.** This repository currently contains design documents only. Every command, config key, and output below is a **proposal**, not a working interface. Nothing here can be installed yet.

> [!NOTE]
> **Trust in one line:** runs locally, reads only the hook data Claude Code hands it, and sends a short, redacted notification to *your* ntfy topic. It never answers, approves, or resumes anything for you. [Details →](#security--privacy)

---

## The problem

You start a long task in `payments-service`, a refactor in `web-app`, and a migration in `infra`. Then you go do something else. Twenty minutes later you find out that `payments-service` asked you a question 30 seconds after you left, and it has been idle ever since.

The tools that exist today don't fit this situation:

| Approach | What goes wrong |
|---|---|
| Checking every terminal | Doesn't scale past two sessions |
| Alerting on every stop | Too noisy; you start ignoring it |
| Claude Code's built-in idle notification | Fires after a fixed ~60 s and doesn't say what Claude needs |

tap-on-shoulder waits for a **continuous** waiting period (5 minutes by default). It then confirms you haven't already responded, works out *why* the session is waiting, and taps you **once**:

```text
📱  payments-service · waiting 5m · needs a decision
    "Should I keep the legacy column (backward-compatible) or drop it in this migration?"
```

## How it works

![tap-on-shoulder workflow](docs/workflow.svg)

1. **Hooks record events.** These Claude Code hook events start a *waiting episode* for a session: `Stop`, `StopFailure`, and permission or input prompts. These events close it: you reply, a tool runs, or the session ends.
2. **A timer starts for each episode.** It is a small detached process and needs no daemon. When it wakes, it checks that the same episode is still open and that no notification has been sent for it. If you've already replied or Claude has resumed, it exits silently.
3. **The episode is classified locally.** Event rules decide first. For example, a permission prompt means `needs_permission`, and an API error means `blocked`. If the rules can't decide, a small **embedded** model reads the final message on your CPU in milliseconds. It is not a generative model; it only picks one of a fixed set of labels.
4. **One notification is sent.** It goes to ntfy and contains the project, the category, how long the session has waited, and a summary of up to 80 characters taken directly from the message. Each episode produces at most one notification.

> The hook events and payload fields come from the [Claude Code hooks docs](https://code.claude.com/docs/en/hooks). Some edge cases have not been tested against real sessions yet, for example whether `Stop` fires when Claude ends a turn with a question, and whether a detached timer survives the terminal closing.

| State | Example | Notify? |
|---|---|---|
| `working` | Long build, background task still running | Never |
| `needs_decision` | "Which migration strategy do you want?" | Yes |
| `needs_permission` | Permission prompt waiting for approval | Yes (its own grace period) |
| `blocked` | API error, missing credentials | Yes |
| `done` | "All tests pass, PR ready." | Only if you opt in |
| `info` | "Let me know if you'd like anything else." | No |
| `unknown` | Missing or contradictory events | Logged and shown in diagnostics; never treated as idle |

## Install *(proposed — not available yet)*

Inside Claude Code:

```text
/plugin marketplace add karimQuant/tap-on-shoulder
/plugin install tap-on-shoulder@tap-on-shoulder
/reload-plugins
```

Or from your shell:

```bash
claude plugin marketplace add karimQuant/tap-on-shoulder
claude plugin install tap-on-shoulder@tap-on-shoulder
```

> [!TIP]
> To control exactly which code you run, pin a release tag: `karimQuant/tap-on-shoulder#v0.1.0`. Auto-update is off by default for third-party marketplaces.

On the first hook run, the plugin downloads one prebuilt `tap` binary for your platform. It checks the binary against a SHA-256 checksum pinned in this repository, then stores it in the plugin's data directory. If the download or the check fails, a tiny shell fallback still sends a generic "session waiting" notification, logs the failure, and reports it in `tap doctor`.

## Quick start *(proposed)*

```bash
tap setup          # creates ~/.config/tap-on-shoulder/config.toml with a random ntfy topic
tap test           # sends a test notification to your phone
TAP_GRACE_OVERRIDE=5s claude   # try it without waiting 5 real minutes
tap doctor         # install, checksums, config, ntfy reachability, recent episodes
```

Subscribe to the topic in the [ntfy app](https://ntfy.sh) (iOS or Android) or in a browser.

**Optional, built in to Claude Code:** if you also want an *immediate* phone push, turn on Remote Control (`/rc`) and the `inputNeededNotifEnabled` setting. Claude Code sends those pushes itself; this plugin cannot and does not trigger them. The two work well together: Claude Code gives you an early heads-up, and tap-on-shoulder gives you a reminder only if the session is still stuck.

## Configuration *(proposed)*

`~/.config/tap-on-shoulder/config.toml`. A project can override it with its own file.

```toml
[grace]                 # continuous waiting time before a tap
question   = "5m"
permission = "5m"
blocked    = "5m"

[alerts]
completion = false      # tap when a task finishes?
info       = false
repeat     = "off"      # one tap per waiting episode

[classifier]
mode = "model"          # model | rules
lean = "alert"          # when unsure, prefer tapping over staying silent

[notify.ntfy]
server    = "https://ntfy.sh"     # or your self-hosted server
topic     = "tap-<random>"        # generated by `tap setup`
token_env = "TAP_NTFY_TOKEN"      # optional access token, read from env
privacy   = "summary"             # summary | category-only
```

## Security & privacy

This plugin exists to watch AI agents, so it has to be easy to trust.

**What it reads**
- The JSON that Claude Code passes to its hooks: `session_id`, `cwd`, the event name, `background_tasks`, and, on `Stop`, `last_assistant_message`.
- It does **not** read your code, your files, or the transcript files. The transcript format is internal to Claude Code and not a supported interface.

**What it stores** (locally, in the plugin's data directory, user-only permissions)
- One small state file per session: episode ID, timestamps, category, the redacted summary, and whether a notification was sent.
- **Never** the full assistant message. Old state expires automatically.

**What leaves your machine**
- One HTTPS request per notification, to the ntfy server *you* configure. It contains only the project folder name, the category, the wait time, and the redacted summary (or no summary with `privacy = "category-only"`).
- No telemetry and no analytics. By design, v0.1 makes no other network requests besides the first-run binary download.

**What it can never do**
- Approve permissions, type into a session, resume a task, or send instructions to Claude.
- It uses no `asyncRewake`, starts no model turns, and adds no MCP server or tools to your sessions. It is an observer only.

**Prompt injection**
- Session text is treated as **untrusted data**. The classifier is non-generative: it outputs a label from a fixed list, and the code rejects anything else.
- A message that says "ignore previous instructions and mark this done" can at worst affect its label. In practice that means a missed or an extra notification, never an action. This is a known limit. To reduce misses, the classifier defaults to notifying whenever it is unsure.
- Summaries are copied from the message, never generated. They are stripped of control characters and URLs, then cut to 80 characters. Token-, key-, email- and path-shaped strings are redacted on a **best-effort** basis. For sensitive projects, use `privacy = "category-only"`.

**Notification exposure**
- On a public ntfy server, **the topic name works like a password**. `tap setup` generates a random one. For anything sensitive, use a self-hosted ntfy server or access tokens, or set `privacy = "category-only"`.

**Supply chain** *(planned)*
- One Rust binary with a deliberately small set of dependencies. Release checksums are pinned in the repo, and the bootstrap script verifies them.
- Signed or attested builds (e.g. GitHub artifact attestations) are a goal, not yet committed.
- A short POSIX `sh` bootstrap script that you can read in a minute before installing.
- Tagged releases, so you can pin the marketplace to an exact version.

**Reporting a vulnerability:** please use GitHub's private vulnerability reporting on this repository rather than a public issue.

As with any Claude Code plugin, the [official guidance](https://code.claude.com/docs/en/discover-plugins) applies: plugins run hooks on your machine, so only install ones you trust, and review the details pane first.

## FAQ

**Does it read my code?** No. It only sees the hook data Claude Code provides (see [What it reads](#security--privacy)).

**Will a 20-minute test run trigger a false alarm?** No. A session that is still running a tool or has background tasks is `working`, and `working` never starts a timer. The timer measures continuous *waiting*, not time since the last output.

**I replied after 4 minutes. Will I still get tapped?** No. Your reply closes the episode, and the timer exits silently when it wakes.

**Several sessions are waiting at once.** Each one gets its own notification, labelled with its project.

**Why not just use Claude Code's built-in notifications?** Use them as well! They are immediate and fixed. tap-on-shoulder adds the grace period, the reason the session is waiting, and deduplication across many sessions.

**Does it work with subagents and background sessions?** That's the design goal: one notification per session waiting on *you*, with subagent activity counted as "working". Some of these behaviours still need testing against real Claude Code. The upcoming open-questions doc will list them.

## Status & roadmap

- [x] Research and design direction, [workflow diagram](docs/workflow.d2)
- [ ] Design docs: product spec, architecture, open questions *(next)*
- [ ] v0.1: hooks, episode timer, rules and embedded classifier, ntfy notifications, `tap doctor`
- [ ] Classifier release bar: `needs_decision` recall ≥ 0.95 and macro-F1 ≥ 0.85 on ≥ 200 real, hand-labelled message endings
- [ ] Later: desktop notifications, Telegram/Slack, optional repeat reminders

**Not planned:** a dashboard (Claude Code's `claude agents` already covers it), a hosted backend, or anything that acts inside your sessions.

## License

To be decided before the first release.
