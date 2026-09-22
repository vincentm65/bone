#!/usr/bin/env bash
# Drives the ask_user_swallow example under tmux and captures each phase.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/../../target/debug/examples/ask_user_swallow"
OUT="$DIR/captures"
S=askuser-repro

[ -x "$BIN" ] || { echo "binary not found: $BIN (run: cargo build --example ask_user_swallow)" >&2; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT/markers"

tmux kill-session -t "$S" 2>/dev/null || true
tmux new-session -d -s "$S" -x 100 -y 24
tmux set-option -t "$S" remain-on-exit on

tmux send-keys -t "$S" "REPRO_MARKER_DIR=$OUT/markers $BIN" Enter

wait_marker() {
  local m="$1"
  for _ in $(seq 1 300); do
    [ -f "$OUT/markers/$m" ] && return 0
    sleep 0.1
  done
  echo "timed out waiting for $m" >&2
  tmux capture-pane -p -t "$S" -S -100 > "$OUT/timeout_$m.txt"
  return 1
}

wait_marker phase_a
tmux capture-pane -p -t "$S" -S -100 > "$OUT/phase_a.txt"

wait_marker phase_a2
sleep 0.5
tmux capture-pane -p -t "$S" -S -100 > "$OUT/phase_a2.txt"

wait_marker phase_b
tmux capture-pane -p -t "$S" -S -100 > "$OUT/phase_b.txt"

wait_marker phase_c
sleep 0.5
tmux capture-pane -p -t "$S" -S -100 > "$OUT/phase_c.txt"

tmux kill-session -t "$S" 2>/dev/null || true

report() {
  local name="$1" file="$2"
  echo "=== $name ==="
  grep -n "SENTINEL" "$file" || echo "(SENTINEL NOT VISIBLE)"
}

report "Phase A: idle pane, sentinel visible above viewport"        "$OUT/phase_a.txt"
report "Phase A2: clamping condition injected (region 1..20)"       "$OUT/phase_a2.txt"
report "Phase B: ask_user menu opened, sentinel SWALLOWED"          "$OUT/phase_b.txt"
report "Phase C: hard reset + re-flush, sentinel RESTORED"          "$OUT/phase_c.txt"

echo
echo "captures in $OUT"
