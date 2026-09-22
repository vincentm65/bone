#!/usr/bin/env bash
# Probe tmux DECSTBM semantics: does CUD (CSI B) scroll when the cursor sits
# BELOW the bottom of an active DECSTBM scrolling region?
#
# vt100/xterm semantics: it must NOT scroll (it clamps at the screen bottom),
# because only a cursor *inside* the region at the region's bottom row triggers
# a region scroll. This is the behavior the ask_user-swallow demo relies on:
# a leftover DECSTBM region makes tmux's CUD behave like a clamping terminal.
#
# Method: a pane running `cat` (echo disabled) reflects raw bytes to the pane
# output, so `send-keys -l` sequences are processed by tmux as terminal
# OUTPUT, exactly like app escape sequences.
set -uo pipefail

S=decstbm-test
tmux kill-session -t "$S" 2>/dev/null || true
tmux new-session -d -s "$S" -x 100 -y 24 'bash -c "stty -echo; exec cat"'
sleep 0.5

out=$'\033[2J'
for i in 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17 18; do
  out+=$'FILLER-'"$i"$'\n'
done
out+=$'SENTINEL-CLAMP-TEST\n'
for i in 20 21 22 23; do
  out+=$'FILLER-'"$i"$'\n'
done
# Row 24 (screen bottom), then re-home the cursor to row 24 col 1.
out+=$'\033[24;1HFILLER-24'
out+=$'\033[24;1H'
tmux send-keys -t "$S" -l "$out"
sleep 0.5

echo "=== baseline (no region; sentinel on row 19) ==="
tmux capture-pane -t "$S" -p | cat -n | sed -n '17,24p'

# DECSTBM region rows 1..20 (1-based): excludes the bottom row (24) where the
# cursor sits. Then CUD x3 from the bottom row.
tmux send-keys -t "$S" -l $'\033[1;20r'
sleep 0.3
tmux send-keys -t "$S" -l $'\033[3B'
sleep 0.3

echo "=== after ESC[1;20r + CUD x3 ==="
tmux capture-pane -t "$S" -p | cat -n | sed -n '17,24p'
n=$(tmux capture-pane -t "$S" -p -S -20 | grep -c 'SENTINEL-CLAMP-TEST' || true)
echo "sentinel occurrences in (history + visible): $n  (1 => clamped/no scroll, 2 => scrolled)"

tmux kill-session -t "$S" 2>/dev/null || true
