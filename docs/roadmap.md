# Roadmap

## Now: design

- [x] Concept and research ([verified interfaces](design/verified-interfaces.md))
- [x] Proposed workflow ([diagram](assets/workflow.svg))
- [x] [Product spec](design/product-spec.md), [architecture](design/architecture.md), [model](design/model.md), and [open questions](design/open-questions.md)
- [x] Spike: confirm the hook behaviours against real Claude Code
- [ ] Record the real demo (terminal and phone side by side)

## v0.1 alpha: the first tap

- [x] Plugin hooks that open and close waiting episodes, bound to tool calls
- [x] One on-demand sweeper with at-most-once delivery, dedup, and restart handling
- [x] Event rules plus embedded classifier (model2vec + linear head, preview)
- [x] ntfy notifications with redaction and privacy modes
- [x] `/tap-on-shoulder:setup`, `:test`, `:doctor`; `tap classify` and `tap label` in the terminal
- [x] Real end-to-end test against Claude Code (`scripts/e2e.sh`)
- [x] First signed binary prerelease (`bin-v0.1.0-alpha.1`)
- [ ] Release bar for the classifier: `needs_decision` recall ≥ 0.95 and macro-F1 ≥ 0.85 on at least 200 real, hand-labelled message endings (collect them with `training.collect` + `tap label`)

## v0.2: away-aware taps and the idle ledger

- [x] Away-aware taps: screen locked or idle ≥ 5 min ⇒ tap after `grace.away` (30 s); SSH and unknown keep normal timing (GNOME, macOS)
- [x] Idle ledger and `tap stats` / `/tap-on-shoulder:stats`: blocked-on-you time, local only, no message text

## Later

- Answerable taps: show the question's options in the tap (answering from the phone would need its own design; see *Not planned*)
- Permission allow-list suggestions drawn from the ledger
- Presence on KDE and other desktops
- An opt-in holdout that measures whether taps shorten waits
- Desktop notifications, Telegram, Slack, and Discord
- Optional repeat reminders
- A stronger classifier (Ettin encoder) if the model2vec model misses the release bar

## Not planned

- **A dashboard.** Claude Code's `claude agents` view already covers it.
- **A hosted backend** or cloud classification.
- **Anything that acts inside your sessions**, such as answering, approving, or resuming.
