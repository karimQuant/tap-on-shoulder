# Architecture

**Status:** draft for v0.1. Everything here is **Proposed** unless it links to [verified-interfaces.md](verified-interfaces.md).

## Overview

```text
Claude Code session ──hook JSON──▶ scripts/tap-hook.sh ──exec──▶ tap hook <Event>
                                                                   │
                     ${CLAUDE_PLUGIN_DATA}/state/<session>.json ◀──┤ open / update / close episode
                                                                   │
                                          (on open) spawn detached ▼
                                                     tap wait <session> <episode>
                                                         │ wall-clock loop
                                                         ▼
                                      still same open, un-notified episode past due?
                                                         │ yes
                                                         ▼
                                              ntfy HTTPS POST (once)
```

There are no daemons and no hosted parts: one Rust binary (`tap`) plus one POSIX shell shim.

## Components

| Component | Responsibility |
|---|---|
| `hooks/hooks.json` | Registers command hooks for the events below. All of them call the shim |
| `scripts/tap-hook.sh` | Finds the verified binary and `exec`s it. If the binary is missing, it starts a background install and returns immediately |
| `tap hook <Event>` | Reads hook JSON from stdin, applies the event mapping, updates state under a lock, and spawns the timer when needed. Must return in ≤ 20 ms at p95 |
| `tap wait` | Detached per-episode timer. Wakes on wall-clock time, re-checks state, and sends at most once |
| Classifier | Event rules first, then the embedded model (see [model.md](model.md)). Pure function: text and event in, category and summary out |
| Notifier | `trait Notifier { fn send(&self, tap: &Tap) -> Result<()> }`. v0.1 implements ntfy only |
| `tap setup / test / doctor / config check / label` | User-facing CLI |

## Event mapping

<a id="event-mapping"></a>

| Hook event (matcher) | Action |
|---|---|
| `SessionStart` | Close any open episode for this session. Prune state older than 24 h. Warn the user if the binary is unavailable |
| `UserPromptSubmit` | **Close** (the user responded) |
| `PreToolUse` with `AskUserQuestion` or `ExitPlanMode` | **Open** `needs_decision`. The summary comes from the tool input (the question or plan title) |
| `PreToolUse` with any other tool | **Close** (activity) |
| `PostToolUse` / `PostToolUseFailure` | **Close** (activity; includes an answered `AskUserQuestion`) |
| `Notification` with `permission_prompt` | **Open** `needs_permission`. The summary comes from `message` |
| `Notification` with `elicitation_dialog` | **Open** `needs_decision` |
| `PermissionDenied` | **Close** |
| `Stop` with non-empty `background_tasks` | No episode. The session is still `working` |
| `Stop` (otherwise) | Classify `last_assistant_message`, then **open** with the resulting category |
| `StopFailure` | **Open** `blocked` |
| `SessionEnd` | **Close** and delete the session's state |

Other events (`idle_prompt`, `SubagentStart/Stop`, and so on) are ignored. Events that come from inside a subagent (with `agent_id` set) count as activity for the parent session, so they can only **close** an episode. The one exception is `AskUserQuestion`, which still opens one.

**Re-arming:** if an arming event arrives while an episode is already open, the episode keeps its `opened_at` and `episode_id`. Only the category and summary are updated. Continuous waiting is measured from the first arming event, and only one timer runs.

## State

Each session has one file: `${CLAUDE_PLUGIN_DATA}/state/<session_id>.json`. The session ID is checked against `^[A-Za-z0-9_-]{1,128}$`, and anything else is rejected. Directory mode is 0700 and file mode 0600.

```json
{
  "version": 1,
  "session_id": "…",
  "project": "payments-service",
  "claude_pid": 12345,
  "episode": {
    "id": "01J…",
    "status": "open | closed | notified | failed",
    "category": "needs_decision",
    "summary": "Keep the legacy column or drop it?",
    "opened_at": 1761436800,
    "due_at": 1761437100,
    "timer_pid": 12399,
    "attempts": 0
  },
  "updated_at": 1761436800
}
```

- **Writes:** atomic (temporary file, `fsync`, `rename`) under an exclusive `flock` on `<session_id>.lock`. Hooks for the same session can run concurrently, so every read-modify-write holds the lock.
- **Never stored:** the full assistant message. `summary` is already redacted.
- **Corrupt or unreadable file:** the session is treated as `unknown`, the problem is logged, the file is replaced on the next arming event, and **no tap** is sent.

## Timer ownership

- On **open**, `tap hook` spawns `tap wait <session> <episode_id>` in a new session (`setsid`), with stdio sent to `/dev/null`, and then returns. Hooks never wait for the timer.
- **The loop:** read the state. Exit if the episode ID differs, or the status is not `open`. Otherwise `sleep(min(due_at − now, 30 s))` and repeat. Wall-clock checks make it safe across suspend and resume.
- **When due:** take the lock, re-check, set `status = notified` (claim before send), release, and send.
  - If the send fails, set `status = failed` and retry up to 3 times with backoff within 2 minutes.
  - At-most-once wins over at-least-once: a crash between claiming and sending loses one tap rather than duplicating it.
- **Session liveness:** at open, record the PID of the nearest ancestor process named `claude`, if one is found. At fire time, if that PID is gone, the episode is closed as `session_gone` and no tap is sent. If no ancestor was found, the check is skipped. **Unverified:** how Claude Code launches hook processes.
- **After a reboot:** timers are gone, and so are the sessions. Stale state is pruned on the next `SessionStart`, and leftover episodes are never sent.

## Configuration loading

1. **User config:** `${XDG_CONFIG_HOME:-~/.config}/tap-on-shoulder/config.toml`, which may set every key.
2. **Project config:** `<cwd>/.claude/tap-on-shoulder.toml` may set **only** `grace.*`, `alerts.*`, and `classifier.lean`. Any `notify.*`, `classifier.mode`, or `training.*` key in a project file is ignored with a warning. This stops a cloned repository from redirecting your notifications or enabling data collection.
3. **Environment:** `TAP_GRACE_OVERRIDE` replaces every grace period (testing only). The ntfy token is read from the variable named in `token_env`.

## Security boundaries

- **Trust boundary:** hook stdin JSON (including `last_assistant_message` and tool input) is untrusted. It is parsed with `serde` into typed structs with size limits: stdin is capped at 1 MiB and messages are truncated to 16 KiB before classification.
- **No shell interpolation:** session text is never placed in a command line, an environment variable, or a file path.
- **Fixed outputs:** the classifier outputs an enum. The summary passes through `sanitize`, which removes control and bidi characters and URLs, redacts secrets, emails, and paths, and truncates to 80 characters.
- **Network:** the only egress is the configured ntfy URL, over HTTPS, unless the user explicitly configures an `http://` server for localhost or a LAN. The HTTP client uses `rustls` and has a 10 s timeout.
- **No exit code 2, ever:** `tap hook` always exits 0 and prints nothing to stdout, except for `SessionStart` warnings. It can never block, alter, or feed text into Claude.

## Install and distribution

- **Plugin repo:** `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json`, `hooks/hooks.json`, `scripts/`, plus the Rust source.
- **Binary releases:** release tags `bin-vX.Y.Z` build `tap` for `x86_64` and `aarch64` on Linux (musl) and macOS. The build attaches artifact attestations.
- **Checksums:** `scripts/checksums.txt` pins the SHA-256 of every binary. It is committed **after** the binary release, and the plugin release tag `vX.Y.Z` points at that commit.
- **First hook run:** the shim downloads the binary for the current platform into `${CLAUDE_PLUGIN_DATA}/bin/`, checks it against the pinned checksum, and moves it into place. This runs in the background behind a lock directory. A checksum mismatch deletes the download and logs an error.
- **Development:** `TAP_BIN=/path/to/tap` skips the download.
- **Model:** the model weights are embedded in the binary, so there is a single artifact with a single checksum.

## Observability

- **Log:** `${CLAUDE_PLUGIN_DATA}/log/tap.log`, JSON lines, rotated at 1 MiB, 3 files kept. Each line records the event, the session (shortened ID), the episode ID, the transition, the category, and errors. Message text is **never** logged.
- **`tap doctor`:** reports binary version and checksum, config validity and ignored keys, ntfy reachability (a HEAD request), currently open episodes with their due times, recent failures, and the classifier mode.

## Failure handling

| Failure | Behaviour |
|---|---|
| Binary missing or unverified | The shim logs it and starts the background install. `SessionStart` shows a one-line warning. No taps until the install succeeds |
| Model fails to load | Rules-only classification. Undecided `Stop` messages become `needs_decision` (the lean-alert default). Logged |
| Config invalid | Built-in defaults are used, except ntfy: with no valid topic, nothing can be sent, so `doctor` and the `SessionStart` warning report it |
| ntfy fails | Retried up to 3 times, then `failed`. Logged and shown in `doctor` |
| State write fails (disk full, permissions) | Logged. The hook still exits 0, and the session is `unknown` |
| Clock jumps | `due_at` is absolute wall-clock time. A backwards jump delays the tap, a forwards jump sends it early. Accepted |

## Testing strategy

- **Unit tests:** the event mapping, the classifier rules, `sanitize` and redaction, config precedence, and the at-most-once claim logic.
- **Integration tests:** spawn the real binary with fixture hook JSON on stdin, `TAP_GRACE_OVERRIDE=1s`, a temporary `CLAUDE_PLUGIN_DATA`, and a local mock ntfy server. Each acceptance scenario in [product-spec.md](product-spec.md#acceptance-scenarios) has one test.
- **Classifier parity:** the Rust inference matches the Python training pipeline on fixtures (absolute difference in probabilities < 1e-4).
- **Manual end-to-end:** a script that runs a real `claude` session with the plugin loaded through `--plugin-dir` and a short grace period.
