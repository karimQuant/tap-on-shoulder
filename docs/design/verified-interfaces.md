# Verified interfaces

These Claude Code capabilities were checked against the official documentation on **2026-10-25** (schema re-checked on 2026-10-26), and the spike results below against Claude Code 2.1.310. Claude Code changes quickly, so re-check before relying on any detail.

## Plugins

| Fact | Source |
|---|---|
| The manifest is `.claude-plugin/plugin.json`, and only `name` is required. Components live at the plugin root, not inside `.claude-plugin/` | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| A plugin can bundle hooks (`hooks/hooks.json`), skills, commands, agents, MCP servers, and monitors | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| `${CLAUDE_PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_DATA}` are available to hooks. The data directory survives plugin updates | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| Install with `/plugin marketplace add OWNER/REPO` and `/plugin install NAME@MARKETPLACE`, or the `claude plugin …` equivalents | [Discover plugins](https://code.claude.com/docs/en/discover-plugins) |
| Third-party marketplaces don't auto-update by default | [Discover plugins](https://code.claude.com/docs/en/discover-plugins) |

## Hooks

| Fact | Source |
|---|---|
| Common input fields include `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, `agent_id`, `agent_type` | [Hooks](https://code.claude.com/docs/en/hooks) |
| `Stop` input includes `last_assistant_message`, `stop_hook_active`, `background_tasks`, `session_crons` | [Hooks](https://code.claude.com/docs/en/hooks) |
| `Stop` doesn't fire on a user interrupt. API errors fire `StopFailure` instead | [Hooks](https://code.claude.com/docs/en/hooks) |
| `Notification` matchers include `permission_prompt`, `idle_prompt`, and `elicitation_dialog` | [Hooks](https://code.claude.com/docs/en/hooks) |
| `idle_prompt` fires about 60 s after Claude finishes, if you haven't typed. The docs give no way to configure that delay | [Hooks](https://code.claude.com/docs/en/hooks) |
| Command hooks time out after 600 s by default. `SessionEnd` hooks have a budget of about 1.5 s | [Hooks](https://code.claude.com/docs/en/hooks) |
| `async: true` runs a command hook in the background. Matching hooks run in parallel | [Hooks](https://code.claude.com/docs/en/hooks) |
| An `asyncRewake` hook that exits with code 2 wakes Claude and starts a turn. **We never use it** | [Hooks](https://code.claude.com/docs/en/hooks) |

## Remote Control and notifications

| Fact | Source |
|---|---|
| Remote Control (`/rc`, `--rc`, `claude remote-control`) lets the Claude app or claude.ai/code follow a local session | [Remote Control](https://code.claude.com/docs/en/remote-control) |
| `inputNeededNotifEnabled` pushes on permission prompts and questions, and `agentPushNotifEnabled` pushes when Claude decides. Both work only while Remote Control is connected | [Settings](https://code.claude.com/docs/en/settings-reference) |
| No documented way exists for a hook or plugin to trigger those pushes, so tap-on-shoulder sends its own notifications | [Remote Control](https://code.claude.com/docs/en/remote-control) |

## Sessions

| Fact | Source |
|---|---|
| `/clear`, `/branch`, and `--fork-session` produce new session IDs | [Sessions](https://code.claude.com/docs/en/sessions) |
| The transcript JSONL format is internal and changes between versions, so we don't parse it | [Sessions](https://code.claude.com/docs/en/sessions) |
| `claude agents` (agent view) already lists sessions by state. We don't build a dashboard | [Agent view](https://code.claude.com/docs/en/agent-view) |

## Context

- A feature request for delayed hooks that cancel when the session resumes was closed as *not planned* ([anthropics/claude-code#22889](https://github.com/anthropics/claude-code/issues/22889)). That is the gap this plugin fills.

## Manifest and hook schema (re-checked 2026-10-26)

| Fact | Source |
|---|---|
| `plugin.json` fields used here: `name` (kebab-case, required), `version` (string, not checked against semver; pins users to it), `description`, `author` (object: `name` required, optional `email` and `url`), `homepage` (must parse as a URL), `repository` (string, not validated), `keywords` (array of strings). `license` is an SPDX string; it is left out until the owner decides | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| Unknown top-level `plugin.json` keys are stripped with a `claude plugin validate` warning; `--strict` turns warnings into failures | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| `${CLAUDE_PLUGIN_DATA}` resolves to `~/.claude/plugins/data/<id>/`, kept across updates and deleted on the last uninstall. `${CLAUDE_PLUGIN_ROOT}` changes on every update, so no state goes there | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| Hook processes get `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA`, and `CLAUDE_PROJECT_DIR` in their environment; `${...}` resolves anywhere in `command` and `args` | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| `marketplace.json` requires `name`, `owner` (object with `name`), and `plugins[]`; each entry needs `name` and `source`. A relative `source` is resolved from the marketplace root (the directory holding `.claude-plugin/`) and must not contain `..` | [Create a marketplace](https://code.claude.com/docs/en/plugin-marketplaces) |
| `hooks/hooks.json` wraps the event map in a top-level `hooks` key and may carry an optional top-level `description` | [Hooks](https://code.claude.com/docs/en/hooks) |
| Exec form: a command hook with `args` spawns `command` directly with `args` as argv, no shell. Without `args` it runs through `sh -c`. We use exec form | [Hooks](https://code.claude.com/docs/en/hooks) |
| `timeout` is in seconds. `SessionEnd` hooks share a 1.5 s budget, raised to the longest per-hook `timeout` (at most 60 s) | [Hooks](https://code.claude.com/docs/en/hooks) |
| Matchers: `"*"`, `""` or omitted match all. Strings made only of letters, digits, `_`, `-`, spaces, `,` and `\|` are exact names or lists; anything else is an unanchored JavaScript regex. `UserPromptSubmit` and `Stop` ignore matchers | [Hooks](https://code.claude.com/docs/en/hooks) |
| Matcher targets: tool name for `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `PermissionDenied`; notification type for `Notification`; error type for `StopFailure`; MCP server name for `ElicitationResult`; reason for `SessionEnd` | [Hooks](https://code.claude.com/docs/en/hooks) |
| `systemMessage` in hook JSON output is a warning shown to the user | [Hooks](https://code.claude.com/docs/en/hooks) |
| A top-level `bin/` directory stops claude.ai and Cowork from installing a plugin. We ship none | [Plugins reference](https://code.claude.com/docs/en/plugins-reference) |
| `claude plugin validate .` checks `marketplace.json`, `plugin.json` and `hooks/hooks.json`, and works without credentials | Local run, Claude Code 2.1.310 |

## Spike results (Claude Code 2.1.310, 2026-10-26)

Tested with a logging plugin loaded via `--plugin-dir`, in `-p` mode and in interactive tmux sessions. Only field names and shapes are recorded.

| # | Observed shape | Used for |
|---|---|---|
| S1 | `Stop` fires when a turn ends in a plain-text question. Payload: `last_assistant_message` (string), `background_tasks[]` of `{id, type, status, description, command}`, `session_crons`, `stop_hook_active` (bool) | Arming on `Stop` |
| S2 | A finished background task produces a synthetic `UserPromptSubmit` whose `prompt` starts with `<task-notification>`, then a new `Stop` with `background_tasks: []` | Skip arming while a task is `running` |
| S3 | `SubagentStop` (with `agent_id`) follows `Stop` even without user-visible subagents | `SubagentStop` is not registered |
| S4 | Process chain: hook → `sh` → `claude` (comm `claude`) | Liveness: walk up to 4 ancestors |
| S5 | `CLAUDE_PLUGIN_DATA` is set even with `--plugin-dir` (`…/plugins/data/<name>-inline`) | State dir always available; e2e pins it through a `TAP_BIN` wrapper |
| S6 | A `setsid` child survives the hook exiting and the Claude process exiting | Detached sweeper and background install |
| S7 | Permission flow: `PreToolUse{tool_use_id}` → `PermissionRequest{tool_name, tool_input}` (no `tool_use_id`) → about 6 s later `Notification{notification_type: "permission_prompt", message}`. Nothing fires on approval; the next event is `PostToolUse{tool_use_id}` when the tool finishes | Bind by `tool_name` + `input_hash`; running-tool check |
| S8 | `AskUserQuestion`: `PreToolUse{tool_use_id, tool_input.questions[].question}` → `PermissionRequest` → `Notification permission_prompt` → `PostToolUse{tool_use_id, answers}` | Close on the matching `tool_use_id` |
| S9 | `SessionEnd.reason` is `other` for `-p`; `SessionStart.source` is `startup` | `-p` runs close on `SessionEnd` |
| S10 | Esc or "No" at a permission prompt fires no hook event at all (no `PermissionDenied`, `PostToolUseFailure`, `Stop`, or `idle_prompt` within 80 s) | Known limitation: one possible tap after cancel-and-walk-away |

Also confirmed on 2.1.310: exec-form plugin hooks (`command` = `${CLAUDE_PLUGIN_ROOT}/scripts/tap-hook.sh`, `args` = `[<Event>]`) reach the shim with the hook JSON on stdin and `CLAUDE_PLUGIN_DATA` set. A `claude -p` run fires `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`. The folder-trust dialog lists "No, exit" first, then "Yes, I trust this folder".

## Unverified

These assumptions are still open:

- [ ] Does a detached process survive the machine sleeping? (S6 covers the terminal and Claude exiting.)
- [ ] Is `async` really ignored for `Stop` hooks? Not relied on: `hooks.json` never sets `async`.
- [ ] Is `session_id` stable across a plain `--resume` or `--continue`?
- [ ] Does the `Notification` hook fire in the VS Code extension? There are reports that it doesn't.
- [ ] Do Claude Code's built-in Ghostty, Kitty, and iTerm2 notifications cause double alerts?

Have you tested one of these? Open an issue with your Claude Code version and what you saw.
