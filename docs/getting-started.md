# Getting started

> [!WARNING]
> **Proposed.** This is the target install experience. It does not work yet.

## Requirements

- Claude Code with plugin support.
- Linux or macOS on x64 or arm64.
- The [ntfy](https://ntfy.sh) app on your phone (iOS or Android), or a browser.
- Optional: Claude Code's Remote Control (`/rc`) for Claude's own immediate pushes.

You don't need Python, Node, or a GPU. The plugin downloads one small prebuilt binary.

## 1. Install the plugin

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
> Pin an exact release with `karimQuant/tap-on-shoulder#v0.1.0` so you know what code runs. Third-party marketplaces don't auto-update by default. Update with `claude plugin update tap-on-shoulder@tap-on-shoulder`.

The first time a hook runs, the plugin downloads the `tap` binary for your platform. It checks the binary against a SHA-256 checksum pinned in this repository before using it. See [Security](security.md#supply-chain).

## 2. Connect your phone

```bash
tap setup
```

This creates `~/.config/tap-on-shoulder/config.toml` with a **random** ntfy topic and prints the subscribe URL. Open that URL in a browser, or add the topic in the ntfy app.

## 3. Send a test tap

```bash
tap test
```

Your phone should show a test notification within a few seconds.

## 4. Try it without waiting 5 minutes

```bash
TAP_GRACE_OVERRIDE=10s claude
```

Ask Claude something that ends with a question, then don't answer. About 10 seconds later you get a tap. Reply before then, and nothing is sent.

## 5. Optional: turn on Remote Control pushes

Claude Code can push to the Claude mobile app by itself while Remote Control is connected. Turn on `/rc` and the `inputNeededNotifEnabled` setting. Those pushes are immediate and come from Claude Code, not from this plugin. tap-on-shoulder is the follow-up when a session is still waiting.

## Something wrong?

```bash
tap doctor
```

`tap doctor` checks the install, the binary and model checksums, your config, whether your ntfy server is reachable, and recent waiting episodes. Then see the [FAQ](faq.md).
