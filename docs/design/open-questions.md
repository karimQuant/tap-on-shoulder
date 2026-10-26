# Open questions

Each question has a **recommended default**, which is what v0.1 implements unless evidence changes it. Answers backed by tests should link the evidence.

| # | Question | Recommended default | Rationale |
|---|---|---|---|
| 1 | Does a detached `setsid` timer survive the terminal closing? | Assume yes. Verify in the e2e spike | `setsid` detaches from the controlling terminal, so SIGHUP doesn't reach it. If Claude itself dies, the liveness check suppresses the tap |
| 2 | Is `async` ignored on `Stop` hooks? | Don't depend on `async`. The hook returns in milliseconds after spawning the timer | Correct either way |
| 3 | Does `Stop` fire when Claude ends a turn with a plain-text question? | Assume yes ("main agent finishes responding"). Verify | If it doesn't, plain-text questions would be missed. That would be a blocking bug for v0.1 |
| 4 | Is `session_id` stable across `--resume`? | Doesn't matter: `SessionStart` closes any open episode for the incoming ID, and old IDs are pruned after 24 h | The design is correct in both cases |
| 5 | How does Claude Code spawn hook processes (for the liveness check)? | Look for an ancestor process named `claude`. Skip the check if none is found | Best effort. A false "session gone" would suppress a tap, so the check runs only with a confident match |
| 6 | Same grace for permissions? | Configurable, `5m` default | User decision (interview round 4) |
| 7 | Completion taps? | Off by default | User decision. Completion is not a request for action |
| 8 | Fallback when the binary is missing? | Warn at `SessionStart` and log. **No** shell-based taps | A full fallback means re-implementing dedup and timers in `sh`, which adds surface for little gain. The install normally completes in seconds |
| 9 | Delivery channel | ntfy only. Built-in Remote Control pushes (`/rc` + `inputNeededNotifEnabled`) are documented as a separate, user-enabled channel | User decision (interview round 1). A plugin can't trigger `/rc` pushes |
| 10 | Model architecture | model2vec `potion-base-8M` + logistic regression head, embedded | Smallest artifact that is still accurate. See [model.md](model.md) |
| 11 | Re-notify on long waits? | Off. One tap per episode | Avoid alert fatigue. `alerts.repeat` is reserved for later |
| 12 | Group simultaneous taps into a digest? | No | Rare in practice. Separate taps carry clearer context |
| 13 | Project config changing `notify.*` | Forbidden | Prevents exfiltration through a cloned repo's config |
| 14 | What if the user replies from the Remote Control phone app? | `UserPromptSubmit` should still fire in the local session. Verify | If it doesn't, the phone reply leaves the episode open and the user gets a tap anyway. Low impact |
| 15 | VS Code extension support | Not targeted for v0.1 | Reports say `Notification` hooks don't fire there |
| 16 | Do Claude Code's built-in terminal notifications cause double alerts? | Accept it. They're local, ours goes to the phone, and they have different purposes | |
| 17 | Licence | MIT, to match model2vec | Permissive and simple. **Needs owner confirmation** |
| 18 | `http://` ntfy servers | Allowed only for `localhost`, private IP ranges, or `.local` hosts | Keeps the topic and summary off plaintext public networks |
