# Roadmap

## Now: design

- [x] Concept and research ([verified interfaces](design/verified-interfaces.md))
- [x] Proposed workflow ([diagram](assets/workflow.svg))
- [ ] Product spec, architecture, and open questions in `docs/design/`
- [ ] Spike: confirm the unverified hook behaviours against real Claude Code
- [ ] Record the real demo (terminal and phone side by side)

## v0.1: the first tap

- [ ] Plugin hooks that open and close waiting episodes
- [ ] Detached per-episode timer with dedup and restart handling
- [ ] Event rules plus embedded classifier (model2vec)
- [ ] ntfy notifications with redaction and privacy modes
- [ ] `tap setup`, `tap test`, `tap doctor`, and `TAP_GRACE_OVERRIDE` for testing
- [ ] Release bar for the classifier: `needs_decision` recall ≥ 0.95 and macro-F1 ≥ 0.85 on at least 200 real, hand-labelled message endings

## Later

- Desktop notifications, Telegram, Slack, and Discord
- Optional repeat reminders
- A stronger classifier (Ettin encoder) if the model2vec model misses the release bar

## Not planned

- **A dashboard.** Claude Code's `claude agents` view already covers it.
- **A hosted backend** or cloud classification.
- **Anything that acts inside your sessions**, such as answering, approving, or resuming.
