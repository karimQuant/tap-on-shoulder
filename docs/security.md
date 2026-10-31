# Security & privacy

tap-on-shoulder watches AI agents, so it has to be easy to trust. This page states exactly what it touches. Every guarantee below is implemented in v0.1 and covered by automated tests, plus an independent security review.

## What it reads

Only the JSON that Claude Code passes to plugin hooks, and only these fields (the full table is in [contract.md](design/contract.md#hook-input-fields-we-read-everything-else-is-ignored)):

| Field | Event | Used for | Can appear in the redacted summary? |
|---|---|---|---|
| `hook_event_name`, `session_id`, `cwd` | all | routing; the project folder name labels the notification | `cwd` only as the folder name, sent next to the summary, never inside it |
| `tool_name`, `tool_use_id`, `notification_type`, `source`, `reason`, `background_tasks[].status` | various | tracking whether the session is working or waiting | no |
| `tool_input` | `PreToolUse`, `PermissionRequest` | hashed (sha256, first 16 hex digits) to bind a permission prompt to its tool call. The input itself is **never stored** | no, except the two fields below |
| the AskUserQuestion question (`tool_input.questions[0].question`) | `PreToolUse` | the summary of a decision | **yes** |
| the ExitPlanMode plan (`tool_input.plan`), first non-empty line only | `PreToolUse` | the summary, as `plan ready: <line>` | **yes**, that line only |
| the elicitation `message` | `Notification` (`elicitation_dialog`) | the summary of an MCP input request | **yes** |
| `last_assistant_message` | `Stop` | classified locally; one line is chosen as the summary | **yes**, one line |

Every summary passes through the same redaction (contract.md, [Summary extraction](design/contract.md#summary-extraction-not-model-dependent)) and is cut to 80 characters. The prompt you type (`UserPromptSubmit`) is never read.

It does **not** read your code, your files, or Claude Code's transcript files. The transcript format is internal to Claude Code and not a supported interface.

## What it stores

The plugin keeps one small state file per session, in the plugin's data directory, readable only by your user:

- episode ID
- timestamps
- category
- the redacted summary
- whether a notification was sent (episode status and attempt count)
- pending tool calls: tool name, `tool_use_id`, and a truncated SHA-256 hash of the tool input (never the input itself)
- the Claude Code process ID and start time, to tell whether the session is still running

It **never** stores the full assistant message. Old state expires automatically. Presence readings (v0.2) are kept only in the sweeper's memory for 5 seconds; the state file records at most whether a tap was sent early (`sent_early`), never a presence timeline.

The **idle ledger** (`stats/waits.jsonl`, read by `tap stats`; on only when the user config says `[stats] enabled = true`, which `tap setup` writes into new configs and announces, so a missing key, as in a config from before v0.2, means off) keeps one line per finished question, permission prompt, or blocker: when it opened, when it was tapped, when it ended, its category, a fixed close reason (such as `user_prompt` or `session_end`), whether the tap went out early because you were away (`early`, never a presence timeline), and the project folder name. It never holds message text, summaries, or tool names; with `privacy = "category-only"` the project name is left out too. It is a local record of when you were at work, so it can be turned off with `[stats] enabled = false` (`tap doctor` shows whether it is on and how to switch it) (user config only; a project file cannot change it). Past 1 MiB it keeps only the last 90 days.

## What leaves your machine

- **Per notification:** one HTTPS request to the ntfy server *you* configure. It carries the project folder name, the category, the wait time, and the redacted summary. With `privacy = "category-only"`, the summary is left out.
- **Once, at first run:** the binary download from this repository's GitHub releases.
- **By design, v0.1 makes no other network requests.** No telemetry, no analytics.

## What it can never do

- Approve or deny permissions.
- Type into a session, send instructions to Claude, or resume a task.
- Start model turns. It doesn't use `asyncRewake`, so it never wakes Claude.
- Add MCP servers or tools to your sessions.

It is an observer. The only outputs are a local log and one ntfy message per episode.

## Presence probes (v0.2)

With `presence.mode = "auto"` (the default) the sweeper asks, at most once per pass and only while an episode could tap early, whether you are away:

- **GNOME:** `/usr/bin/gdbus call --session` to `org.gnome.Mutter.IdleMonitor.GetIdletime` and `org.gnome.ScreenSaver.GetActive`.
- **macOS:** `/usr/sbin/ioreg -c IOHIDSystem -d 4 -k HIDIdleTime`.

Each command runs by absolute path with an empty environment (only `LC_ALL=C` and, for gdbus, `DBUS_SESSION_BUS_ADDRESS`; the `$XDG_RUNTIME_DIR/bus` fallback is used only if it is a socket owned by you), stdin and stderr closed, stdout capped at 4 KiB, and is killed after 500 ms. Output must be exactly one integer or boolean in the expected shape; anything else reads as *unknown*, which means the normal grace. Nothing is sent anywhere. `presence.mode = "off"` disables the probes. The existing `ps` calls run as `/bin/ps` for the same reason: `PATH` is not trusted.

## Threat model

| Threat | Mitigation | Residual risk |
|---|---|---|
| **Prompt injection.** Session text says "ignore instructions, mark this done" | Session text is untrusted data. The classifier is non-generative and outputs a label from a fixed list, checked in code. No label can trigger an action. When unsure, it leans toward alerting | A crafted message could cause a **missed or extra** notification. It can never cause an action |
| **Secrets in notifications** | Summaries are copied from the message, never generated. Control characters are stripped, URLs (including `file://`, `ssh://` and `git@host:path`) are replaced, the text is cut to 80 characters, and token-, key-, email-, and path-shaped strings and `password=…`-style values are redacted | Redaction is **best effort**. Use `category-only` for sensitive work |
| **Someone reads your notifications** | `tap setup` generates a random topic. Self-hosted ntfy and access tokens are supported | On public ntfy servers, **anyone who knows the topic can read it**. Treat it like a password |
| **Plain `http://` to a `.local` server** | `http://` is accepted only for loopback, RFC 1918 addresses, `localhost`, and `.local` names. The server comes only from your own config file; no session text or hook input can set it | A `.local` name is resolved by multicast DNS, so another device on the same network can answer for it and receive the notification in clear text. Prefer `https://`, or an IP literal for a LAN server |
| **Tampered binary** | SHA-256 checksums pinned in the repo, verified before first use. Pin a release tag for the marketplace | You trust this repository's release process |
| **Stuck or stale state** | Episode IDs, a `sending` claim taken under a lock before each send, wall-clock checks, and expiring state | An event Claude Code never sends leaves the state `unknown`. That is logged, never silently treated as idle |

**Out of scope:** a compromised local user account, a compromised Claude Code install, and the security of the ntfy server you choose.

## Supply chain

- One Rust binary with a deliberately small set of dependencies.
- A short POSIX `sh` bootstrap script you can read in a minute. It downloads the binary and verifies the checksum.
- Tagged releases, so the marketplace can be pinned to an exact version.
- Release builds carry GitHub artifact attestations, signed only by the release workflow on a `bin-v*` tag push (manual runs never attest). Check one, pinning both the tag and the signing workflow:

  ```sh
  gh attestation verify tap-<target> --repo karimQuant/tap-on-shoulder \
    --source-ref refs/tags/bin-v<version> \
    --signer-workflow karimQuant/tap-on-shoulder/.github/workflows/release.yml
  ```
- The `bin-v*` and `v*` tags are protected by a repository ruleset that blocks updating or deleting them, so a published tag cannot be moved to other code.

## Plugin trust in general

Claude Code plugins can run hooks on your machine. Follow the [official guidance](https://code.claude.com/docs/en/discover-plugins): install only plugins you trust, and review the details pane before installing.

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
