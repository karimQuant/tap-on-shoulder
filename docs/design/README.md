# Design notes

This folder holds the engineering side of tap-on-shoulder: what we verified, what we decided, and what is still open. It's written for contributors.

| Document | Status | Contents |
|---|---|---|
| [Verified interfaces](verified-interfaces.md) | ✅ Available | Claude Code capabilities we rely on, each with a documentation link, plus the list of unverified assumptions |
| [Product spec](product-spec.md) | ✅ v0.1 | Goals, non-goals, user stories, settings, notification policy, acceptance scenarios |
| [Architecture](architecture.md) | ✅ v0.1 | Components, event mapping, state, timer ownership, security boundaries, failure handling |
| [Classifier model](model.md) | ✅ v0.1 (preview) | Model architecture, data sources, training pipeline, preview gate, release bar |
| [Open questions](open-questions.md) | 📝 Draft | Unresolved decisions, each with a recommended default and rationale |

## Principles

1. **Supported interfaces only.** We use plugin hooks and documented payload fields. No terminal scraping and no parsing of internal transcript files.
2. **Observe, never act.** The plugin cannot approve, answer, resume, or wake a session.
3. **Deterministic first.** Code owns events, timers, cancellation, dedup, and delivery. The model only labels text.
4. **Session content is untrusted data.** It never turns into instructions.
5. **Small and local.** One binary, no backend, minimal retention.
6. **Label every claim.** Each statement is marked **Verified** (with a link), **Proposed**, or **Unverified**.

## Proposing a change

Open an issue with the `design` label. Describe the problem first, then the proposal. If your change depends on Claude Code behaviour, link the official documentation, or say it is unverified.
