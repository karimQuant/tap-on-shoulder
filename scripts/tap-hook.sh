#!/bin/sh
# tap-on-shoulder hook shim. hooks/hooks.json runs:  tap-hook.sh <Event>
#
# Hot path: find the `tap` binary and exec `tap hook <Event>` (stdin passes
# through). Resolution order:
#   1. $TAP_BIN, if executable (development / tests)
#   2. ${CLAUDE_PLUGIN_DATA}/bin/tap-<version>, <version> pinned on the first
#      line of scripts/checksums.txt, accepted only once its .verified marker
#      exists (written after a successful SHA-256 check)
#   3. ${CLAUDE_PLUGIN_ROOT}/target/release/tap, only when TAP_DEV=1
#
# No binary yet: drain stdin, start a detached background install (this same
# script with --install), print a one-line systemMessage on SessionStart only,
# and exit 0. A hook never blocks, never exits non-zero, never waits on the
# network.
#
# scripts/tap.sh (the slash-command and terminal wrapper) shares this logic:
#   tap-hook.sh --resolve       print the usable binary path, or exit 1
#   tap-hook.sh --install-now   install synchronously, messages on stderr;
#                               exit 0 installed, 3 nothing pinned,
#                               4 no CLAUDE_PLUGIN_DATA, 1 failed
#
# The install downloads the release asset for this platform into the data dir,
# verifies it against the pinned checksum, chmod 0500, then renames it into
# place. It never evals anything; every expansion is quoted.
set -u

ROOT=${CLAUDE_PLUGIN_ROOT:-}
if [ -z "$ROOT" ]; then
  ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd) || exit 0
fi
DATA=${CLAUDE_PLUGIN_DATA:-}
CHECKSUMS=$ROOT/scripts/checksums.txt
RELEASES=https://github.com/karimQuant/tap-on-shoulder/releases/download

# Pinned version from line 1 ("version=<v>"); no subprocess on the hot path.
VERSION=
if [ -r "$CHECKSUMS" ]; then
  IFS= read -r first_line <"$CHECKSUMS" || true
  case ${first_line:-} in
    version=*) VERSION=${first_line#version=} ;;
  esac
fi
case $VERSION in
  '' | *[!0-9A-Za-z.+-]*) VERSION= ;;
esac

BIN_DIR=$DATA/bin
LOCK=$BIN_DIR/.install.lock

NOW=0

log() {
  [ "$NOW" = 1 ] && printf 'tap: %s\n' "$*" >&2
  [ -n "$DATA" ] || return 0
  mkdir -p "$DATA/log" 2>/dev/null || return 0
  printf '%s install: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$DATA/log/install.log" 2>/dev/null || true
}

detect_target() {
  os=$(uname -s 2>/dev/null) || return 1
  arch=$(uname -m 2>/dev/null) || return 1
  case $os in
    Linux) os_part=unknown-linux-musl ;;
    Darwin) os_part=apple-darwin ;;
    *) return 1 ;;
  esac
  case $arch in
    x86_64 | amd64) arch_part=x86_64 ;;
    aarch64 | arm64) arch_part=aarch64 ;;
    *) return 1 ;;
  esac
  printf '%s-%s\n' "$arch_part" "$os_part"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | { read -r sum _ && printf '%s\n' "$sum"; }
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -- "$1" | { read -r sum _ && printf '%s\n' "$sum"; }
  else
    return 1
  fi
}

# Expected sha256 for asset "$1" from checksums.txt, or empty.
pinned_sum() {
  want=$1
  found=
  while read -r sum name rest; do
    case $sum in '' | '#'* | version=*) continue ;; esac
    if [ "$name" = "$want" ] && [ -z "${rest:-}" ]; then
      found=$sum
    fi
  done <"$CHECKSUMS"
  case $found in
    *[!0-9a-f]*) found= ;;
  esac
  [ "${#found}" -eq 64 ] || found=
  printf '%s' "$found"
}

# Exit status: 0 installed (or nothing done in background mode), 3 nothing to
# install (no pinned release, unsupported platform), 4 no data dir, 1 failure.
install_binary() {
  if [ -z "$DATA" ]; then
    log "CLAUDE_PLUGIN_DATA is not set; nowhere to install"
    exit 4
  fi
  umask 077
  mkdir -p "$BIN_DIR" "$DATA/log" || exit 1

  # Lock: mkdir is atomic. A lock older than 10 minutes is stale.
  if ! mkdir "$LOCK" 2>/dev/null; then
    if [ -n "$(find "$LOCK" -prune -mmin +10 2>/dev/null)" ]; then
      log "removing stale lock"
      rm -rf -- "$LOCK"
      mkdir "$LOCK" 2>/dev/null || exit 0
    else
      log "another install is running; try again in a minute"
      exit 1
    fi
  fi
  tmp=
  trap 'rm -rf -- "$LOCK"; [ -z "$tmp" ] || rm -f -- "$tmp"' EXIT
  trap 'exit 1' HUP INT TERM

  # At most one attempt per 10 minutes, so a missing release is not retried
  # (and logged) on every hook. An explicit --install-now skips the throttle.
  stamp=$BIN_DIR/.install.attempt
  if [ "$NOW" != 1 ] && [ -e "$stamp" ] &&
    [ -z "$(find "$stamp" -mmin +10 2>/dev/null)" ]; then
    exit 0
  fi
  : >"$stamp"

  if [ -z "$VERSION" ]; then
    log "no release pinned (checksums.txt has no version line)"
    exit 3
  fi
  if ! target=$(detect_target); then
    log "unsupported platform: $(uname -s 2>/dev/null) $(uname -m 2>/dev/null)"
    exit 3
  fi
  asset=tap-$target
  expected=$(pinned_sum "$asset")
  if [ -z "$expected" ]; then
    log "no release pinned for $asset at version $VERSION"
    exit 3
  fi

  dest=$BIN_DIR/tap-$VERSION
  url=$RELEASES/bin-v$VERSION/$asset
  tmp=$(mktemp "$BIN_DIR/.tap-download.XXXXXX") || {
    log "cannot create temp file"
    exit 1
  }
  log "downloading $asset $VERSION"
  if ! curl -fsSL --proto '=https' --tlsv1.2 --retry 2 --connect-timeout 15 --max-time 300 \
    -o "$tmp" "$url"; then
    log "download failed: $url"
    exit 1
  fi
  if ! actual=$(sha256_of "$tmp") || [ -z "$actual" ]; then
    log "no sha256sum or shasum available"
    exit 1
  fi
  if [ "$actual" != "$expected" ]; then
    log "checksum mismatch for $asset $VERSION (expected $expected, got $actual); download deleted"
    exit 1
  fi
  if ! chmod 0500 "$tmp" || ! mv -f -- "$tmp" "$dest"; then
    log "cannot move binary into place"
    exit 1
  fi
  tmp=
  marker_tmp=$(mktemp "$BIN_DIR/.tap-marker.XXXXXX") || exit 1
  printf '%s  %s\n' "$expected" "$asset" >"$marker_tmp" && mv -f -- "$marker_tmp" "$dest.verified"
  rm -f -- "$stamp"
  log "installed $asset $VERSION"
  exit 0
}

resolve_bin() {
  bin=
  if [ -n "${TAP_BIN:-}" ] && [ -x "$TAP_BIN" ]; then
    bin=$TAP_BIN
  elif [ -n "$DATA" ] && [ -n "$VERSION" ] && [ -x "$BIN_DIR/tap-$VERSION" ] &&
    [ -f "$BIN_DIR/tap-$VERSION.verified" ]; then
    bin=$BIN_DIR/tap-$VERSION
  elif [ "${TAP_DEV:-}" = "1" ] && [ -x "$ROOT/target/release/tap" ]; then
    bin=$ROOT/target/release/tap
  fi
}

case ${1:-} in
  --install) install_binary ;;
  --install-now)
    NOW=1
    install_binary
    ;;
  --resolve)
    resolve_bin
    [ -n "$bin" ] || exit 1
    printf '%s\n' "$bin"
    exit 0
    ;;
esac

event=${1:-}
resolve_bin

if [ -n "$bin" ]; then
  [ -n "$event" ] || exit 0
  exec "$bin" hook "$event"
fi

# No usable binary: never block Claude.
cat >/dev/null 2>&1 || true
if [ -n "$DATA" ] && [ ! -d "$LOCK" ]; then
  if command -v setsid >/dev/null 2>&1; then
    setsid nohup sh "$0" --install </dev/null >/dev/null 2>&1 &
  else
    nohup sh "$0" --install </dev/null >/dev/null 2>&1 &
  fi
fi
if [ "$event" = "SessionStart" ]; then
  printf '%s\n' '{"systemMessage":"tap-on-shoulder: installing its binary in the background; taps start once it finishes (check with /tap-on-shoulder:doctor)."}'
fi
exit 0
