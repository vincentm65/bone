#!/usr/bin/env sh
set -eu

session="$(tmux show-option -gqv @super-popup-session)"
width="$(tmux show-option -gqv @super-popup-width)"
height="$(tmux show-option -gqv @super-popup-height)"

[ -n "$session" ] || session="__tmux_super_popup"
[ -n "$width" ] || width="96%"
[ -n "$height" ] || height="92%"

if [ "${1:-}" != "--toggle" ]; then
  exit 0
fi

current="$(tmux display-message -p '#{session_name}')"
if [ "$current" = "$session" ]; then
  exec tmux detach-client
fi

exec tmux display-popup -E -w "$width" -h "$height" "tmux new-session -A -s '$session'"
