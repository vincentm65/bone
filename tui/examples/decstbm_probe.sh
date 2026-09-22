#!/usr/bin/env bash
# Compare actual CUD and newline OUTPUT, with/without restricted DECSTBM margins.
set -euo pipefail

if [[ ${1:-} == --pane ]]; then
  out="$2"; region="$3"; movement="$4"
  stty raw -echo
  sync_output() {
    local reply
    printf '\033[6n'
    IFS= read -rs -d R -t 5 reply
  }
  printf '\033[2J\033[H'
  for ((row = 1; row <= 24; row++)); do
    printf '\033[%d;1HROW-%02d' "$row" "$row"
  done
  [[ "$region" == restricted ]] && printf '\033[1;20r'
  # DECSTBM homes the cursor. Reposition AFTER setting the margins, below them.
  printf '\033[21;1H'
  sync_output
  touch "$out/baseline"
  for ((i = 0; i < 400; i++)); do
    [[ -f "$out/continue" ]] && break
    sleep 0.05
  done
  [[ -f "$out/continue" ]] || exit 1
  if [[ "$movement" == CUD ]]; then
    printf '\033[12B'
  else
    for ((i = 0; i < 12; i++)); do printf '\n'; done
  fi
  sync_output
  touch "$out/moved"
  sleep 30
  exit 0
fi

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/../.." && pwd)"
mkdir -p "$ROOT/target"
OUT="$(mktemp -d "$ROOT/target/decstbm-probe.XXXXXXXX")"
SOCKET="$OUT/tmux.sock"
mux() { tmux -S "$SOCKET" -f /dev/null "$@"; }
cleanup() { mux kill-server 2>/dev/null || true; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fail() { echo "FAIL: $* (captures: $OUT)" >&2; exit 1; }
wait_marker() {
  for ((i = 0; i < 400; i++)); do
    [[ -f "$1" ]] && return 0
    sleep 0.05
  done
  fail "timed out waiting for $1"
}

for region in full restricted; do
  for movement in CUD LF; do
    run="$OUT/$region-$movement"
    SOCKET="$run/tmux.sock"
    mkdir -p "$run"
    printf -v launch 'bash %q --pane %q %q %q' "$DIR/decstbm_probe.sh" "$run" "$region" "$movement"
    mux new-session -d -s probe -x 100 -y 24 "$launch"
    wait_marker "$run/baseline"
    [[ $(mux display-message -p -t probe '#{pane_width}x#{pane_height}') == 100x24 ]] || fail 'wrong pane size'
    [[ $(mux display-message -p -t probe '#{cursor_x},#{cursor_y},#{history_size}') == 0,20,0 ]] || fail 'wrong baseline geometry'
    bounds=0,23
    [[ "$region" == restricted ]] && bounds=0,19
    [[ $(mux display-message -p -t probe '#{scroll_region_upper},#{scroll_region_lower}') == "$bounds" ]] || fail 'wrong scroll region'
    mux capture-pane -p -t probe > "$run/before.txt"
    touch "$run/continue"
    wait_marker "$run/moved"
    mux capture-pane -p -t probe > "$run/after.txt"
    history=$(mux display-message -p -t probe '#{history_size}')
    cursor=$(mux display-message -p -t probe '#{cursor_x},#{cursor_y}')
    sentinel_row=$(grep -nx 'ROW-19' "$run/after.txt" | cut -d: -f1)
    expected_history=0; expected_row=19
    if [[ "$region" == full && "$movement" == LF ]]; then
      expected_history=9; expected_row=10
    fi
    [[ "$history" == "$expected_history" && "$sentinel_row" == "$expected_row" && "$cursor" == 0,23 ]] || \
      fail "$region/$movement: history=$history row=$sentinel_row cursor=$cursor"
    echo "PASS $region/$movement: history=$history, ROW-19 now at row $sentinel_row, cursor=$cursor (zero-based)"
    mux kill-session -t probe
  done
done

echo "captures: $OUT"
