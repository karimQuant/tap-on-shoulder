#!/bin/sh
# tap-on-shoulder CLI wrapper:  tap.sh <setup|test|doctor|stats|classify|label|--version> [args]
#
# The plugin's slash commands (commands/*.md) run this, and so can a terminal.
# It finds the `tap` binary exactly as the hook shim does (tap-hook.sh
# --resolve: $TAP_BIN, then the verified ${CLAUDE_PLUGIN_DATA}/bin/tap-<ver>,
# then target/release/tap with TAP_DEV=1). With no binary it runs the same
# verified install synchronously (tap-hook.sh --install-now), then execs it.
# Only the subcommands above pass through; `hook` and `sweep` belong to hooks.
set -u

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd) || exit 1
shim=$here/tap-hook.sh

case ${1:-} in
  setup | test | doctor | stats | classify | label | --version) ;;
  *)
    printf '%s\n' 'usage: tap.sh <setup [--force] [--server URL] | test | doctor | stats [--days N] [--json] | classify | label | --version>' >&2
    exit 2
    ;;
esac

if ! bin=$(sh "$shim" --resolve); then
  printf '%s\n' 'tap-on-shoulder: the tap binary is not installed yet; installing the pinned, checksum-verified release now.' >&2
  sh "$shim" --install-now </dev/null
  status=$?
  if [ "$status" -eq 4 ]; then
    cat >&2 <<'EOF'
tap-on-shoulder: CLAUDE_PLUGIN_DATA is not set, so there is no install location.
Run /tap-on-shoulder:doctor inside Claude Code, or set TAP_BIN to a tap binary.
EOF
    exit 1
  fi
  if [ "$status" -eq 3 ]; then
    cat >&2 <<'EOF'
tap-on-shoulder: no prebuilt binary is available for this plugin version yet
(they ship with the first bin-v* release). Build it from source instead:
  git clone https://github.com/karimQuant/tap-on-shoulder && cd tap-on-shoulder
  scripts/fetch-model.sh && cargo build --release
then start Claude Code with TAP_BIN=<clone>/target/release/tap set.
EOF
    exit 1
  fi
  if [ "$status" -ne 0 ] || ! bin=$(sh "$shim" --resolve); then
    printf '%s\n' 'tap-on-shoulder: install failed (see the messages above).' >&2
    exit 1
  fi
fi

exec "$bin" "$@"
