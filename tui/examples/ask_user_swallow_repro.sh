#!/usr/bin/env bash
# Renderer regression check. Needs cargo, tmux, and standard POSIX text tools.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../.." && pwd)"
BIN="${REPRO_BIN:-$ROOT/target/debug/examples/ask_user_swallow}"
if [[ ${1:-} == --pane ]]; then
  export REPRO_MARKER_DIR="$2" REPRO_SCROLL_REGION="$3"
  exec "$BIN"
fi

[[ -x "$BIN" ]] || { echo "run: cargo build -p bone --example ask_user_swallow" >&2; exit 1; }
mkdir -p "$ROOT/target"
OUT="$(mktemp -d "$ROOT/target/ask-user-swallow.XXXXXXXX")"
SOCKET="$OUT/tmux.sock"
SENTINEL=SENTINEL-LAST-AGENT-LINE-4f9c
mux() { tmux -S "$SOCKET" -f /dev/null "$@"; }
cleanup() { mux kill-server 2>/dev/null || true; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fail() { echo "FAIL: $* (captures: $OUT)" >&2; exit 1; }

wait_marker() {
  local marker="$1"
  for ((i = 0; i < 400; i++)); do
    [[ -f "$marker" ]] && return 0
    sleep 0.05
  done
  mux capture-pane -p -t repro -S - > "$OUT/timeout.txt"
  fail "timed out waiting for $marker"
}

for region in full restricted; do
  run="$OUT/$region"
  SOCKET="$run/tmux.sock"
  mkdir -p "$run/markers"
  printf -v launch 'bash %q --pane %q %q' "$DIR/ask_user_swallow_repro.sh" "$run/markers" "$region"
  mux new-session -d -s repro -x 100 -y 24 "$launch"
  mux set-option -w -t repro remain-on-exit on
  [[ $(mux display-message -p -t repro '#{pane_width}x#{pane_height}') == 100x24 ]] || fail 'wrong pane size'

  for phase in a a2 b c d e; do
    wait_marker "$run/markers/phase_$phase"
    file="$run/phase_$phase.txt"
    mux capture-pane -p -t repro -S - > "$file"
    mux capture-pane -p -t repro > "$run/screen_$phase.txt"
    history=$(mux display-message -p -t repro '#{history_size}')
    bounds=$(mux display-message -p -t repro '#{scroll_region_upper},#{scroll_region_lower}')
    mux display-message -p -t repro \
      '#{pane_width}x#{pane_height} history=#{history_size} cursor=#{cursor_x},#{cursor_y} scroll=#{scroll_region_upper},#{scroll_region_lower}' > "$run/state_$phase.txt"
    [[ $(grep -Fxc "$SENTINEL" "$file") == 1 ]] || fail "$region/$phase: sentinel lost or duplicated"
    grep -Fxq "$SENTINEL" "$run/screen_$phase.txt" || fail "$region/$phase: sentinel not on screen"
    awk -v sentinel="$SENTINEL" '{ print; if ($0 == sentinel) exit }' "$file" > "$run/transcript_$phase.txt"
    cmp -s "$run/transcript_a.txt" "$run/transcript_$phase.txt" || fail "$region/$phase: transcript changed"
    case "$phase" in
      a) initial_history=$history; expected_history=$history; viewport=4 ;;
      a2) expected_history=$initial_history; viewport=4 ;;
      b) expected_history=$((initial_history + 9)); viewport=13 ;;
      c|e) expected_history=$((initial_history + 9)); viewport=13 ;;
      d) expected_history=$((initial_history + 9)); viewport=4 ;;
    esac
    [[ "$history" == "$expected_history" ]] || fail "$region/$phase: history=$history, expected $expected_history"
    grep -Eq "^(.+ )?viewport=$viewport( |$)" "$run/markers/phase_$phase" || fail "$region/$phase: wrong viewport height"
    expected_bounds=0,23
    [[ "$phase" == a2 && "$region" == restricted ]] && expected_bounds=0,19
    [[ "$bounds" == "$expected_bounds" ]] || fail "$region/$phase: scroll region=$bounds"
    echo "PASS $region/$phase: transcript intact, viewport=$viewport, history=$history"
    touch "$run/markers/phase_$phase.continue"
  done
  for ((i = 0; i < 100; i++)); do
    [[ $(mux display-message -p -t repro '#{pane_dead}') == 1 ]] && break
    sleep 0.05
  done
  [[ $(mux display-message -p -t repro '#{pane_dead_status}') == 0 ]] || fail "$region: example did not exit successfully"
  mux kill-session -t repro
done

echo "captures: $OUT"
