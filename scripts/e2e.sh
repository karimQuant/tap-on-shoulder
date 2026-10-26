#!/bin/sh
# End-to-end check against a real Claude Code session (plan R1). LOCAL ONLY:
# it needs Claude auth, tmux and python3, and spends a few cents on haiku.
# Never run in CI.
#
#   scripts/e2e.sh            full run
#   scripts/e2e.sh --dry-run  build + mock ntfy + config + launcher only, no claude
#
# Scenario 1 (interactive, tmux): a prompt whose answer ends in a question
#   -> exactly 1 POST within 40 s, containing the project dir name; then a reply
#   that makes Claude finish with "All done." -> classified `done`, which is
#   suppressed by default (alerts.completion = false) -> no second POST in 8 s.
# Scenario 2 (claude -p): -> 0 new POSTs within 8 s (SessionEnd closes it).
#
# Isolation: the hook's CLAUDE_PLUGIN_DATA is set by Claude Code itself (S5),
# so TAP_BIN points at a wrapper that pins CLAUDE_PLUGIN_DATA and
# XDG_CONFIG_HOME to scratch dirs before exec'ing the release binary.
set -eu

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

REPO=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
FIRST_WAIT=40
QUIET_WAIT=8
TRUST_KEYS=${E2E_TRUST_KEYS:-Down C-m}

say() { printf 'e2e: %s\n' "$*"; }
fail() {
  printf 'e2e: FAIL: %s\n' "$*" >&2
  exit 1
}
need() { command -v "$1" >/dev/null 2>&1 || fail "missing dependency: $1"; }

need python3
need curl
if [ "$DRY_RUN" -eq 0 ]; then
  need tmux
  need claude
fi

# ---------------------------------------------------------------- build
BIN=$REPO/target/release/tap
if [ -f "$REPO/Cargo.toml" ]; then
  say "building release binary"
  (cd "$REPO" && cargo build --release --locked --quiet)
elif [ "$DRY_RUN" -eq 0 ]; then
  fail "no Cargo.toml: the tap binary cannot be built yet"
fi
if [ "$DRY_RUN" -eq 0 ] && [ ! -x "$BIN" ]; then
  fail "release binary not found at target/release/tap"
fi

# ---------------------------------------------------------------- scratch dirs
TMP=$(mktemp -d)
chmod 0700 "$TMP"
DATA=$TMP/plugin-data
XDG=$TMP/config
PROJ=$TMP/tap-e2e-proj-$$
POSTS=$TMP/posts.log
mkdir -p "$DATA" "$XDG/tap-on-shoulder" "$PROJ"
chmod 0700 "$DATA" "$XDG" "$XDG/tap-on-shoulder"
: >"$POSTS"
PROJ_NAME=$(basename -- "$PROJ")
SOCK=tap-e2e-$$
SESSION=tap-e2e
MOCK_PID=

cleanup() {
  status=$?
  if [ "$DRY_RUN" -eq 0 ]; then
    tmux -L "$SOCK" kill-server >/dev/null 2>&1 || true
  fi
  [ -z "$MOCK_PID" ] || kill "$MOCK_PID" >/dev/null 2>&1 || true
  rm -rf -- "$TMP"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# ---------------------------------------------------------------- mock ntfy
# Appends each POST body as one JSON-encoded line to $POSTS.
MOCK_PY='
import http.server, json, sys
out, portfile = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode("utf-8", "replace")
        with open(out, "a") as f:
            f.write(json.dumps(body) + "\n")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b"{\"id\":\"e2e\"}")
    def do_HEAD(self):
        self.send_response(200)
        self.end_headers()
    def log_message(self, *args):
        pass
srv = http.server.HTTPServer(("127.0.0.1", 0), H)
with open(portfile, "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
'
python3 -I -c "$MOCK_PY" "$POSTS" "$TMP/port" &
MOCK_PID=$!
i=0
while [ ! -s "$TMP/port" ]; do
  i=$((i + 1))
  [ "$i" -le 50 ] || fail "mock ntfy did not start"
  sleep 0.1
done
PORT=$(cat "$TMP/port")
say "mock ntfy on 127.0.0.1:$PORT"

posts() { wc -l <"$POSTS" | tr -d ' '; }

# ---------------------------------------------------------------- config + launcher
cat >"$XDG/tap-on-shoulder/config.toml" <<EOF
[alerts]
completion = false
info = false

[notify.ntfy]
server = "http://127.0.0.1:$PORT"
topic = "tap-e2e"
EOF

WRAPPER=$TMP/tap-wrapper.sh
cat >"$WRAPPER" <<EOF
#!/bin/sh
CLAUDE_PLUGIN_DATA='$DATA'
XDG_CONFIG_HOME='$XDG'
TAP_DEV=1
TAP_GRACE_OVERRIDE=3s
export CLAUDE_PLUGIN_DATA XDG_CONFIG_HOME TAP_DEV TAP_GRACE_OVERRIDE
exec '$BIN' "\$@"
EOF
chmod 0700 "$WRAPPER"

LAUNCH=$TMP/launch.sh
cat >"$LAUNCH" <<EOF
#!/bin/sh
cd '$PROJ' || exit 1
TAP_DEV=1 TAP_GRACE_OVERRIDE=3s TAP_BIN='$WRAPPER' XDG_CONFIG_HOME='$XDG' \\
  exec claude --plugin-dir '$REPO' --model haiku "\$@"
EOF
chmod 0700 "$LAUNCH"

if [ "$DRY_RUN" -eq 1 ]; then
  say "dry run: self-test of the mock"
  curl -fsS -o /dev/null -X POST --data '{"topic":"tap-e2e","message":"self-test"}' "http://127.0.0.1:$PORT/"
  [ "$(posts)" = "1" ] || fail "mock did not record the self-test POST"
  if ! sh -n "$WRAPPER" || ! sh -n "$LAUNCH"; then
    fail "generated scripts do not parse"
  fi
  say "dry run OK (config, wrapper and launcher generated; claude not started)"
  exit 0
fi

# ---------------------------------------------------------------- scenario 1: interactive
say "scenario 1: interactive session in tmux"
tmux -L "$SOCK" new-session -d -s "$SESSION" -x 200 -y 50 "sh '$LAUNCH'"

pane() { tmux -L "$SOCK" capture-pane -p -t "$SESSION" 2>/dev/null || true; }

trusted=0
ready=0
i=0
while [ "$i" -lt 180 ]; do  # up to 90 s: first start can install updates
  screen=$(pane)
  if printf '%s' "$screen" | grep -qi 'trust this folder'; then
    # Folder-trust dialog for the fresh scratch project: "No, exit" is
    # selected first, so Down then Enter picks "Yes, I trust this folder".
    if [ "$trusted" -eq 0 ]; then
      for key in $TRUST_KEYS; do
        tmux -L "$SOCK" send-keys -t "$SESSION" "$key"
        sleep 0.3
      done
      trusted=1
    fi
  elif printf '%s' "$screen" | grep -q 'Claude Code v'; then
    ready=1
    break
  fi
  i=$((i + 1))
  sleep 0.5
done
[ "$ready" -eq 1 ] || fail "claude did not reach the prompt (trust handled: $trusted)"
sleep 2

tmux -L "$SOCK" send-keys -t "$SESSION" -l \
  'Do not use any tools. Reply with one short sentence that ends by asking me which colour I prefer, as a question.'
tmux -L "$SOCK" send-keys -t "$SESSION" C-m

i=0
while [ "$(posts)" -lt 1 ] && [ "$i" -lt "$FIRST_WAIT" ]; do
  i=$((i + 1))
  sleep 1
done
sleep 2
n=$(posts)
[ "$n" -eq 1 ] || fail "expected exactly 1 POST within ${FIRST_WAIT}s, got $n"
grep -qF -- "$PROJ_NAME" "$POSTS" || fail "the POST does not contain the project name"
say "1 POST received after ~${i}s, with the project name"

tmux -L "$SOCK" send-keys -t "$SESSION" -l \
  'Blue. Do not use any tools. Reply with exactly these two words and nothing else: All done.'
tmux -L "$SOCK" send-keys -t "$SESSION" C-m
sleep "$QUIET_WAIT"
n=$(posts)
[ "$n" -eq 1 ] || fail "expected no second POST after the reply (done is suppressed), got $n in total"
say "no second POST after the reply"
tmux -L "$SOCK" kill-server >/dev/null 2>&1 || true

# ---------------------------------------------------------------- scenario 2: claude -p
say "scenario 2: claude -p"
before=$(posts)
sh "$LAUNCH" -p 'Do not use any tools. Ask me which colour I prefer, as a single question.' \
  >/dev/null 2>&1 || fail "claude -p failed"
sleep "$QUIET_WAIT"
after=$(posts)
[ "$after" -eq "$before" ] || fail "claude -p produced $((after - before)) POST(s), expected 0"
say "claude -p produced no POST"

say "PASS"
