#!/bin/sh
# Downloads the pinned potion-base-8M files into assets/potion-base-8M/ and
# verifies each against scripts/model-checksums.txt. Idempotent: files that
# already match are kept, anything else is re-downloaded.
# Usage: scripts/fetch-model.sh
set -eu

REV=bf8b056651a2c21b8d2565580b8569da283cab23
BASE=https://huggingface.co/minishlab/potion-base-8M/resolve/$REV
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SUMS=$ROOT/scripts/model-checksums.txt
DEST=$ROOT/assets/potion-base-8M

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | { read -r sum _ && printf '%s\n' "$sum"; }
  else
    shasum -a 256 -- "$1" | { read -r sum _ && printf '%s\n' "$sum"; }
  fi
}

expected_sum() {
  while read -r sum name; do
    case $sum in '' | '#'*) continue ;; esac
    if [ "$name" = "$1" ]; then
      printf '%s\n' "$sum"
      return 0
    fi
  done <"$SUMS"
  return 1
}

mkdir -p "$DEST"
for file in model.safetensors tokenizer.json config.json; do
  want=$(expected_sum "$file") || {
    printf 'fetch-model: no checksum for %s in %s\n' "$file" "$SUMS" >&2
    exit 1
  }
  if [ -f "$DEST/$file" ] && [ "$(sha256_of "$DEST/$file")" = "$want" ]; then
    printf 'fetch-model: %s ok\n' "$file"
    continue
  fi
  tmp=$(mktemp "$DEST/.$file.XXXXXX")
  trap 'rm -f -- "$tmp"' EXIT
  printf 'fetch-model: downloading %s\n' "$file"
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$tmp" "$BASE/$file"
  got=$(sha256_of "$tmp")
  if [ "$got" != "$want" ]; then
    printf 'fetch-model: checksum mismatch for %s (expected %s, got %s)\n' "$file" "$want" "$got" >&2
    exit 1
  fi
  mv -f -- "$tmp" "$DEST/$file"
  trap - EXIT
done
printf 'fetch-model: assets/potion-base-8M verified\n'
