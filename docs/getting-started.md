# Getting started

> [!IMPORTANT]
> **Prebuilt binaries arrive with the first `bin-v*` prerelease.** Until then, a marketplace install has no `tap` binary to download, and the slash commands print how to build from source. Use [From source](#from-source-development) for now.

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

> [!TIP]
> Pin an exact release with `karimQuant/tap-on-shoulder#v0.1.0` so you know what code runs. Update with `claude plugin update tap-on-shoulder@tap-on-shoulder`.

The plugin downloads the `tap` binary for your platform into its data directory, the first time a hook runs or a slash command needs it. It checks the binary against a SHA-256 checksum pinned in this repository before using it. See [Security](security.md#supply-chain).

## 2. Connect your phone

```text
/tap-on-shoulder:setup
```

This creates `~/.config/tap-on-shoulder/config.toml` with a **random** ntfy topic on `https://ntfy.sh` and prints the subscribe URL. Open that URL in a browser, or add the topic in the ntfy app.

The slash command takes no arguments, because Claude Code inserts arguments into the shell command unescaped. For a self-hosted server, or to replace an existing config, run the binary from a terminal (see [Terminal use](#terminal-use)): `tap setup --server https://ntfy.example.com --force`.

## 3. Send a test tap

```text
/tap-on-shoulder:test
```

Your phone should show a test notification within a few seconds.

## 4. Try it without waiting 5 minutes

```bash
TAP_GRACE_OVERRIDE=10s claude
```

Ask Claude something that ends with a question, then don't answer. About 10 seconds later you get a tap. Reply before then, and nothing is sent.

## 5. Optional: turn on Remote Control pushes

Claude Code can push to the Claude mobile app by itself while Remote Control is connected. Turn on `/rc` and the `inputNeededNotifEnabled` setting. Those pushes are immediate and come from Claude Code, not from this plugin. tap-on-shoulder is the follow-up when a session is still waiting.

## Terminal use

The binary is not on your `PATH`. After a marketplace install it lives in the plugin data directory:

```bash
export CLAUDE_PLUGIN_DATA=~/.claude/plugins/data/tap-on-shoulder-tap-on-shoulder
alias tap="$CLAUDE_PLUGIN_DATA/bin/tap-<version>"   # <version>: first line of scripts/checksums.txt
```

Two commands only work from a terminal:

- `tap classify < file` classifies a message read from stdin. There is no `/classify`, because a slash command can't pass your text to the binary safely.
- `tap label` is interactive and needs a TTY.

## From source (development)

```bash
git clone https://github.com/karimQuant/tap-on-shoulder
cd tap-on-shoulder
scripts/fetch-model.sh
cargo build --release
TAP_BIN="$PWD/target/release/tap" claude --plugin-dir .
```

`TAP_BIN` tells the hooks and the slash commands which binary to run. Then use `/tap-on-shoulder:setup` and `/tap-on-shoulder:test` as above. From a terminal, `scripts/tap.sh <setup|test|doctor|classify|label|--version>` resolves the binary the same way.

## Something wrong?

```text
/tap-on-shoulder:doctor
```

Doctor checks the install, the binary and model, your config, whether your ntfy server is reachable, and recent waiting episodes. If the binary is missing, it tries the verified install first and tells you what failed. Then see the [FAQ](faq.md).
