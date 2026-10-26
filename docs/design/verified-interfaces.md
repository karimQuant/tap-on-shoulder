# Verified interfaces

These Claude Code capabilities were checked against the official documentation on **2026-10-25**. Claude Code changes quickly, so re-check before relying on any detail.

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

## Unverified

These assumptions must be tested against real Claude Code before v0.1:

- [ ] Does a detached sleeper started from a hook survive the terminal closing, or the machine sleeping?
- [ ] Is `async` really ignored for `Stop` hooks? Some community reports say it is.
- [ ] Does `Stop` fire when Claude ends a turn by asking a question in plain text?
- [ ] Is `session_id` stable across a plain `--resume` or `--continue`?
- [ ] Which fields does the `Notification` payload carry besides `message`?
- [ ] Does the `Notification` hook fire in the VS Code extension? There are reports that it doesn't.
- [ ] Do Claude Code's built-in Ghostty, Kitty, and iTerm2 notifications cause double alerts?
- [ ] Is a top-level `bin/` really rejected by claude.ai or Cowork plugin installs?

Have you tested one of these? Open an issue with your Claude Code version and what you saw.
