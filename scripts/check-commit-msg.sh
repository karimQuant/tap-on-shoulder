#!/bin/sh
# Rejects text that leaks personal data: email addresses (other than GitHub
# noreply), absolute home-directory paths, or token-shaped strings.
# Usage: check-commit-msg.sh <file>              (pre-commit commit-msg hook)
#        check-commit-msg.sh --range <rev-range> (CI: check pushed messages)
#        check-commit-msg.sh --files             (CI: scan every tracked file)
set -eu

# One pattern set for every mode.
PAT_EMAIL='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
PAT_HOME='/(home|Users)/[A-Za-z0-9._-]+'
PAT_TOKEN='(^|[^A-Za-z0-9_])(ghp|gho|ghs|github_pat|sk-ant|sk|xox[bp])[-_][A-Za-z0-9_-]{16,}'
PAT_AWS='AKIA[0-9A-Z]{16}'
ALLOW_NOREPLY='noreply@(anthropic\.com|github\.com)|users\.noreply\.github\.com'
# Reserved example domains (RFC 2606) are placeholders, never personal data.
ALLOW_EXAMPLE='@example\.(com|org|net)'

check() {
  # $1: text to check, $2: label for errors
  bad=$(printf '%s\n' "$1" | grep -nE \
    -e "$PAT_EMAIL" -e "$PAT_HOME" -e "$PAT_TOKEN" -e "$PAT_AWS" \
    | grep -vE "$ALLOW_NOREPLY" | grep -vE "$ALLOW_EXAMPLE" || true)
  if [ -n "$bad" ]; then
    printf 'Possible personal data or secret in %s:\n%s\n' "$2" "$bad" >&2
    return 1
  fi
}

if [ "${1:-}" = "--range" ]; then
  status=0
  for rev in $(git rev-list "$2"); do
    check "$(git log -1 --format='%an <%ae>%n%cn <%ce>%n%B' "$rev")" "commit $rev" || status=1
  done
  exit $status
fi

if [ "${1:-}" = "--files" ]; then
  # Allowlist (keep it minimal and explicit):
  #   - this script, which spells out the patterns;
  #   - tests/support/mod.rs, whose fixture builders use placeholder values;
  #   - docs/** lines that are clearly examples: ~/.config, /path/to, example.com;
  #   - lines carrying an explicit `leak-scan: allow` marker (synthetic test
  #     fixtures that must look like secrets to test redaction).
  bad=$(git grep -nIE \
    -e "$PAT_EMAIL" -e "$PAT_HOME" -e "$PAT_TOKEN" -e "$PAT_AWS" \
    -- . ':(exclude)scripts/check-commit-msg.sh' ':(exclude)tests/support/mod.rs' \
    | grep -vE "$ALLOW_NOREPLY" | grep -vE "$ALLOW_EXAMPLE" \
    | grep -v 'leak-scan: allow' \
    | grep -vE '^docs/[^:]*:[0-9]+:.*(~/\.config|/path/to|example\.com)' || true)
  if [ -n "$bad" ]; then
    printf 'Possible personal data or secret in tracked files:\n%s\n' "$bad" >&2
    exit 1
  fi
  exit 0
fi

check "$(grep -v '^#' "$1")" "commit message"
