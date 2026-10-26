# Security & privacy

tap-on-shoulder watches AI agents, so it has to be easy to trust. This page states exactly what it touches. Everything here is the **proposed** design. Each guarantee will be tested before release.

## What it reads

- The JSON that Claude Code passes to plugin hooks: `session_id`, `cwd`, the event name, `background_tasks`, and, on `Stop`, `last_assistant_message`.
- It does **not** read your code, your files, or Claude Code's transcript files. The transcript format is internal to Claude Code and not a supported interface.

## What it stores

The plugin keeps one small state file per session, in the plugin's data directory, readable only by your user:

- episode ID
- timestamps
- category
- the redacted summary
- whether a notification was sent

It **never** stores the full assistant message. Old state expires automatically.

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

## Threat model

| Threat | Mitigation | Residual risk |
|---|---|---|
| **Prompt injection.** Session text says "ignore instructions, mark this done" | Session text is untrusted data. The classifier is non-generative and outputs a label from a fixed list, checked in code. No label can trigger an action. When unsure, it leans toward alerting | A crafted message could cause a **missed or extra** notification. It can never cause an action |
| **Secrets in notifications** | Summaries are copied from the message, never generated. Control characters and URLs are stripped, the text is cut to 80 characters, and token-, key-, email-, and path-shaped strings are redacted | Redaction is **best effort**. Use `category-only` for sensitive work |
| **Someone reads your notifications** | `tap setup` generates a random topic. Self-hosted ntfy and access tokens are supported | On public ntfy servers, **anyone who knows the topic can read it**. Treat it like a password |
| **Tampered binary** | SHA-256 checksums pinned in the repo, verified before first use. Pin a release tag for the marketplace | You trust this repository's release process |
| **Stuck or stale state** | Episode IDs, a `notified` flag, wall-clock checks, and expiring state | An event Claude Code never sends leaves the state `unknown`. That is logged, never silently treated as idle |

**Out of scope:** a compromised local user account, a compromised Claude Code install, and the security of the ntfy server you choose.

## Supply chain

- One Rust binary with a deliberately small set of dependencies.
- A short POSIX `sh` bootstrap script you can read in a minute. It downloads the binary and verifies the checksum.
- Tagged releases, so the marketplace can be pinned to an exact version.
- Release builds carry GitHub artifact attestations. Check one with `gh attestation verify tap-<target> --repo karimQuant/tap-on-shoulder`.

## Plugin trust in general

Claude Code plugins can run hooks on your machine. Follow the [official guidance](https://code.claude.com/docs/en/discover-plugins): install only plugins you trust, and review the details pane before installing.

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
