#!/bin/sh
# Rejects commit messages that leak personal data: email addresses (other than
# GitHub noreply), absolute home-directory paths, or token-shaped strings.
# Usage: check-commit-msg.sh <file>   (pre-commit commit-msg hook)
#        check-commit-msg.sh --range <rev-range>   (CI: check pushed messages)
set -eu

check() {
  # $1: text to check, $2: label for errors
  bad=$(printf '%s\n' "$1" | grep -nE \
    -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' \
    -e '/(home|Users)/[A-Za-z0-9._-]+' \
    -e '(ghp|gho|ghs|github_pat|sk-ant|sk|xox[bp])[-_][A-Za-z0-9_-]{16,}' \
    -e 'AKIA[0-9A-Z]{16}' \
    | grep -vE 'noreply@(anthropic\.com|github\.com)|users\.noreply\.github\.com' || true)
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

check "$(grep -v '^#' "$1")" "commit message"
