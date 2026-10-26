# Security policy

## Reporting a vulnerability

Please **don't** open a public issue. Report privately through GitHub: on the repository's **Security** tab, choose **Report a vulnerability**.

Include what you found, how to reproduce it, and the impact you expect. We aim to acknowledge reports within 72 hours and to agree on a disclosure timeline with you.

## Supported versions

Only the latest release will receive security fixes. There are no releases yet. The project is at the design stage.

## In scope

- Anything that makes the plugin **act** on a Claude Code session: answering, approving, resuming, or waking it.
- Session content that triggers behaviour beyond a classification label, such as prompt injection that escapes the classifier.
- Data leaving the machine beyond what [docs/security.md](docs/security.md) describes.
- Bypassing the binary checksum verification.

## Out of scope

- Missed or extra notifications caused by misclassification. Please file these as regular issues.
- A public ntfy topic being readable by anyone who knows its name. This is documented ntfy behaviour; use a random topic, self-hosting, or access tokens.
- A compromised local account or Claude Code installation.

The full threat model is in [docs/security.md](docs/security.md).
