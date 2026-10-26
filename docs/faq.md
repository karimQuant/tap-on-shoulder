# FAQ

**Does it read my code?**
No. It only sees the hook data Claude Code gives plugins. See [What it reads](security.md#what-it-reads).

**Will a 20-minute test run trigger a false alarm?**
No. A session that is still running a tool, or has background tasks running, is `working`, and that never starts a timer. The timer measures continuous *waiting*, not time since the last output.

**I replied after 4 minutes. Will I still get tapped?**
No. Your reply closes the waiting episode. The timer wakes, sees that, and exits silently.

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
