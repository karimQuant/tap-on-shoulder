# FAQ

**Does it read my code?**
No. It only sees the hook data Claude Code gives plugins. See [What it reads](security.md#what-it-reads).

**Will a 20-minute test run trigger a false alarm?**
No. A session that is still running a tool, or has background tasks running, is `working`, and that never opens a waiting episode. The grace period measures continuous *waiting*, not time since the last output.

**I replied after 4 minutes. Will I still get tapped?**
No. Your reply closes the waiting episode. The pending tap is dropped silently.

**Several sessions are waiting at once. What happens?**
Each one gets its own notification, labelled with its project folder.

**Why not just use Claude Code's built-in notifications?**
Use them too. Claude Code's idle notification fires after a fixed ~60 seconds. Remote Control pushes are immediate. tap-on-shoulder adds three things: a grace period you can configure, the *reason* the session is waiting, and one notification per waiting episode across all your sessions.

**Can it answer Claude or approve permissions from my phone?**
No, by design. It's an observer only. See [What it can never do](security.md#what-it-can-never-do).

**Does it need an API key or send my conversation to a cloud LLM?**
No. Classification runs locally with a small embedded model.

**Telegram, Slack, desktop notifications?**
They're planned after v0.1. The notifier is pluggable. See the [roadmap](roadmap.md).

**Does it work in VS Code, over SSH, or in tmux?**
The terminal CLI is the primary target. Other environments are on the list of things to test in [Verified interfaces](design/verified-interfaces.md#unverified).

**I pressed Esc on a permission prompt and walked away, and later got a tap. Why?**
Claude Code doesn't emit a hook event when you cancel or deny a permission prompt, so the plugin can't tell that you've already handled it. If you type anything afterwards, no tap is sent. This is a known limitation.

**What if a permission prompt is approved and the tool then runs for 20 minutes?**
Before tapping, the plugin checks whether the approved tool's process is running and postpones the tap while it is, for up to 10 minutes. Tools that run inside an existing process, such as some MCP calls, can't be detected and may cause one extra tap.
