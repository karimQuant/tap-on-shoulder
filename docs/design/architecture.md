# Architecture

**Status:** v0.1 design, approved by consensus review. Exact types and formats live in [contract.md](contract.md). The Claude Code behaviour this design relies on was verified by spikes against Claude Code 2.1.310, listed in [verified-interfaces.md](verified-interfaces.md).

## Overview

```text
Claude Code session ──hook JSON──▶ scripts/tap-hook.sh ──exec──▶ tap hook <Event>
                                                                   │ (exits 0 in ms, never blocks Claude)
                     ${CLAUDE_PLUGIN_DATA}/state/<session>.json ◀──┤ open / bind / re-arm / close episode
                                                                   │
                                   (if an episode is open) spawn ▼ (setsid, stdio → /dev/null)
                                                     tap sweep   (one per machine, flock)
                                                         │ sleep ≤ 5 s, wall clock
                                                         ▼
                              same open episode, past due, session alive, tool not running?
                                                         │ yes: claim → send once
                                                         ▼
                                                ntfy HTTPS POST (JSON body)
```

There are no daemons and no hosted parts. The system is one Rust binary (`tap`) and one POSIX shell shim.

## Components

| Component | Responsibility |
|---|---|
| `hooks/hooks.json` | Registers synchronous command hooks (never `async`). Every hook calls the shim |
| `scripts/tap-hook.sh` | Resolves the verified binary and `exec`s it. If no binary is installed yet, it starts a background install and returns immediately |
| `tap hook <Event>` | Parses the hook JSON, applies the event mapping under a per-session lock, and spawns the sweeper when an episode is open |
| `tap sweep` | A single on-demand process. Delivers due episodes at most once, then exits when nothing is left open |
| Classifier | Event rules, then the embedded model ([model.md](model.md)). Output is a category plus an extractive summary |
| Notifier | `trait Notifier`. v0.1 implements ntfy |
| Idle ledger (v0.2) | `src/stats.rs`: one line per finished wait in `${CLAUDE_PLUGIN_DATA}/stats/waits.jsonl` (timings, category, close reason, project folder name; never text), written only through `stats::record` from the hook and the sweeper's `session_gone` close. Off with `[stats] enabled = false` |
| Presence probe (v0.2) | `src/presence.rs`: whether the user is away (screen locked or input idle), read by the sweeper for away-aware taps |
| CLI | `tap setup`, `test`, `doctor`, `stats`, `label`, `classify` (diagnostic: prints category, summary, and probabilities for stdin; writes nothing, sends nothing). The slash commands `/tap-on-shoulder:setup`, `/tap-on-shoulder:test`, `/tap-on-shoulder:doctor`, and `/tap-on-shoulder:stats` (`commands/*.md`) run these through `scripts/tap.sh` |

## Event mapping

Bound episodes are tied to a tool: one call (`tool_use_id`), a set of candidate calls (`candidate_ids`), or a tool name (`bound_tool_name`). Unbound episodes come from the end of a turn.

| Hook event | Action |
|---|---|
| `SessionStart` | Close any open episode, clear `pending_tools`, and prune state older than 24 h. A `.lock` file is deleted only if its `.json` is gone and the lock is older than 24 h. Print a `systemMessage` warning if tap-on-shoulder isn't configured |
| `UserPromptSubmit` | **Close** the episode and clear `pending_tools`. The prompt text is never read |
| `PreToolUse` with `AskUserQuestion` / `ExitPlanMode` | **Open** a **bound** `needs_decision` episode. The summary is the question |
| `PreToolUse` with any other tool | Record a `pending_tools` entry `{id, name, input_hash}`. **Close** unbound episodes |
| `PermissionRequest` | **Open** (or re-arm) a **bound** `needs_permission` episode. It binds to the pending entry with the same name and `input_hash`. Without one, it binds to the only pending entry with that name; to the **set** of them if there are several (`candidate_ids`, `tool_use_id` empty); or to the tool name if there are none (`bound_tool_name`) |
| `Notification` `permission_prompt` | Only sets `confirmed_at` |
| `Notification` `elicitation_dialog` | **Open** `needs_decision` |
| `PostToolUse` / `PostToolUseFailure` | Remove the pending entry. **Close** a bound episode only if its id matches; for a candidate set, only once no candidate is still pending; for a tool name, only if the name matches. **Close** unbound episodes |
| `PermissionDenied` | **Close** a bound episode only if `tool_name` is its tool (the bound call's pending entry, the candidates' name, or `bound_tool_name`). **Close** unbound episodes |
| `ElicitationResult` | **Close** an `elicitation_dialog` episode |
| `Stop` | Clear `pending_tools` and **close** a bound episode. Classify `last_assistant_message` with the user's `classifier.mode` / `classifier.lean`, then **open** (or re-arm) an unbound episode with that category. A `done` or `info` result whose alert is off (`alerts.completion`, `alerts.info`) is closed at once as `suppressed` |
| `Stop` with a `running` background task | Classify as above. `needs_decision` and `blocked` **open** as usual. `done` and `info` mean the session is still working: no episode (a new `Stop` arrives when the task finishes) |
| `StopFailure` | Clear `pending_tools`, **close** a bound episode, and **open** `blocked` (with the same `suppressed` rule as any arming) |
| `SessionEnd` | **Close** the episode and delete the session's state |

**Re-arm.** When an arming event arrives while an episode is already open, the episode keeps its `opened_at`. The category, summary, and binding are replaced only if the new event is bound or has a higher priority (`needs_permission` > `needs_decision` > `blocked` > `done` > `info`).

**Rebinding.** A bound re-arm (`PermissionRequest`, `AskUserQuestion`) whose binding differs from the episode's current binding is a new prompt, so the previous one was answered. The episode starts fresh: `opened_at` and `bound_at` become now, `due_at` is now plus the grace, `postponed_s` is 0, and `confirmed_at` and `retry_at` are cleared. The same binding keeps the plain re-arm behaviour.

**Hook lock.** A hook waits at most 2 s for the session lock. After that it logs `kind=lock_timeout` and exits 0, so a stuck lock never reaches Claude Code's hook timeout.

**Known gap.** Pressing Esc or choosing "No" at a permission prompt fires no hook event at all. If you then walk away without typing, you may still get one tap.

## Sweeper and delivery

- **Exclusivity.** `tap sweep` takes a non-blocking `flock` on `${CLAUDE_PLUGIN_DATA}/sweeper.lock`, so only one runs at a time. The lock lives outside `state/`, so no session id can collide with it. Any hook that leaves an open episode spawns it, and a duplicate exits immediately.
- **Loop.** Scan every session file and sleep `min(earliest_due − now, 5 s)`.
- **Shutdown.** When no episode is open, release the lock and **re-scan once**. Exit only if the second scan is also empty. This closes the race with a hook that opens an episode during shutdown.
- **Policy is applied in the hook.** The `done` / `info` suppression happens at classification time in the hook (see `Stop` above), so the sweeper only ever sees episodes that may tap. At due time it re-checks liveness and delivery state:
- **When an episode is due**, check in order:
  1. **Session gone?** The recorded `claude` process (pid plus start time) has exited or is a zombie → close as `session_gone`. Hooks detect `claude` once per session and store it; they detect again only when the stored process is gone. Every `ps` call runs as `/bin/ps` with `LC_ALL=C TZ=UTC0`.
  2. **Permission tool running?** For `needs_permission`: a process descended from `claude` that started at or after `bound_at + 1 s` is alive in two samples taken 2 s apart → postpone 30 s, up to a total of 10 minutes. The extra second is the minimum time a human takes to approve, so a parallel tool started just before the prompt does not count.
  3. **Claim.** Under the session lock, move `open` → `sending{attempt}`, only if the episode is still the same one and is still due (`now ≥ eff_due`, which is `max(due_at, retry_at)` unless the user is away; see below). The claim records `sent_early`.
  4. **Send**, then record the result:

     | Outcome | New state |
     |---|---|
     | Success | `notified` |
     | Connect error (nothing sent) | Back to `open`, retried after 15 s then 30 s; `failed` (`unreachable`) after the 3rd attempt |
     | HTTP error status | `failed` (`http_status`), not retried |
     | Timeout or dropped connection after the request was written | `sent_uncertain`, not retried |

  The result is written only if the episode is still `sending` with the same attempt. A close that lands during the send always wins.
- **Away-aware taps (v0.2).** Every due check (the scan, the claim re-check, the wake time) uses `eff_due` from [contract.md](contract.md#state-schema): when the user is away, an episode is due at `opened_at + grace.away` instead of `due_at`. Presence comes from `src/presence.rs` (GNOME `gdbus`, macOS `ioreg`; absolute paths, cleared env, 500 ms timeout, strict parsing, cached 5 s). It is probed at most once per pass, and only while an episode is inside its away window. Unknown presence, `presence.mode = "off"`, SSH sessions, and postponed permission taps all keep v0.1 timing. A rebind resets `opened_at`, so it also restarts the away window.
- **Stale claims.** A `sending` state older than 2 minutes becomes `sent_uncertain`. Delivery is **at most once**: we would rather lose a tap than send it twice.

## State

There is one file per session, with the `SessionState` schema from [contract.md](contract.md#state-schema).

- **Location and limits.** `${CLAUDE_PLUGIN_DATA}/state/<session_id>.json`, where `session_id` must match `^[A-Za-z0-9_-]{1,128}$`.
- **Permissions.** Directories are 0700 and must be owned by the user. Files are 0600 and opened with `O_NOFOLLOW`.
- **Writes.** A random temporary file created with `O_EXCL`, then `fsync`, then `rename`, all under an exclusive `flock` on `<session_id>.lock`.
- **Never stored.** The full assistant message, and the user's prompts.
- **Corrupt file.** The session is `unknown`: it is logged, it never taps, and it is replaced on the next arming event.

## Configuration

1. **User config** at `${XDG_CONFIG_HOME:-~/.config}/tap-on-shoulder/config.toml`. Every key is allowed.
2. **Project config** at `<cwd>/.claude/tap-on-shoulder.toml`, at most 64 KiB. Only two kinds of key are honoured: `grace.*`, which can only *shorten* the user's value, and `alerts.completion`. Everything else is ignored with a warning. This means a cloned repository can't redirect notifications, hide taps, or turn on data collection.
3. **Environment.** `TAP_GRACE_OVERRIDE` takes effect only when `TAP_DEV=1` is also set (testing only). The ntfy token is read from the environment variable named in `token_env`.

## Security boundaries

- **Untrusted input.** Hook stdin is untrusted. It is parsed with `serde` into typed structs and capped at 8 MiB. Messages are truncated to 16,384 characters before classification.
- **No session text in commands.** Session text never reaches a command line, an environment variable, a file path, an HTTP header, or the log.
- **Classifier output.** An enum, plus a summary that is copied (never generated), sanitized, and redacted.
- **Network egress.** Only the configured ntfy server, as a JSON POST over HTTPS:
  - `http://` is allowed only for loopback, private-range, or `.local` hosts.
  - Redirects are disabled and the timeout is 10 s.
- **Exit status.** `tap hook` always exits 0, so it can never block or steer Claude.

## Install and distribution

- **Release builds.** A `bin-v*` tag builds `tap` for x86_64 and aarch64 on Linux (musl) and macOS, using native runners. The model must be embedded (`TAP_REQUIRE_MODEL=embed`). Releases include build attestations and a `SHA256SUMS` file.
- **Pinned checksums.** After the binary release, `scripts/checksums.txt` pins the version and the SHA-256 of each binary. The plugin release tag `vX.Y.Z` points at that commit.
- **First hook run.** The shim downloads the binary for the platform into `${CLAUDE_PLUGIN_DATA}/bin/` and verifies the checksum, in the background and behind a lock. A mismatch deletes the download. During development, `TAP_BIN` overrides the binary.

## Observability

- **Log.** `${CLAUDE_PLUGIN_DATA}/log/tap.log`, written as JSON lines and capped at 1 MiB. Each line records the event, a short session ID, the episode ID, the transition, the category, and an error *kind*. Message text is never logged.
- **`tap doctor`.** Reports:
  - version and model variant (with the *preview* note), the user's `classifier.mode` / `classifier.lean`, and the head's `lean_threshold`, `cv_tap_recall`, and `cv_macro_f1`
  - whether the config is valid, and which keys were ignored
  - whether the ntfy server is reachable (exit 1 if not, or if there is no topic or the config is invalid)
  - open episodes and their due times
  - episodes in `failed` or `sent_uncertain`
  - training-data cap warnings
  - the presence source and current reading, and the effective `grace.away`
  - whether the idle ledger is on, with its path and line count

## Testing

| Suite | What it covers | Where it runs |
|---|---|---|
| `tests/scenarios_core.rs` | One named test per acceptance scenario, plus race, corruption, stdout, and log-content tests. Uses a mock ntfy server and `TAP_DEV=1 TAP_GRACE_OVERRIDE=1s` | |
| `tests/scenarios_ledger.rs` | Idle ledger (v0.2): one line per wait, rebind, privacy, off switch, write failures that still tap, concurrent appends with rotation, the `tap stats` report | |
| `tests/scenarios_presence.rs` | Away-aware taps (v0.2), steered by the debug-only `TAP_TEST_PRESENCE*` knobs: early tap when away, present/unknown/off/SSH keep v0.1 timing, claim guard, postponement, probe timeout, config limits | |
| `tests/scenarios_classifier.rs`, `tests/parity.rs` | Rules fallback, prompt-injection cases, and parity with the Python pipeline (identical token ids and labels, probability difference < 1e-3) | |
| `scripts/e2e.sh` | A real interactive `claude` in tmux with the plugin loaded: one tap, no duplicate, no tap after `claude -p` exits | Local only |
