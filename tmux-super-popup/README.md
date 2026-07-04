# tmux-super-popup

Persistent tmux scratch popup.

## Run

From this repo:

```sh
tmux source-file tmux-super-popup/super-popup.tmux
```

Make permanent in `~/.tmux.conf`:

```tmux
run-shell '/home/vincent/projects/bone/tmux-super-popup/super-popup.tmux'
```

## Behavior

- `Alt-a` opens a persistent tmux popup session.
- `Alt-a` inside the popup hides/detaches it.
- The popup session keeps running while hidden, so servers, editors, shells, and logs persist.
- Use normal tmux controls inside the popup to create windows, split panes, and switch tasks.

## Options

```tmux
set -g @super-popup-session __tmux_super_popup
set -g @super-popup-width 96%
set -g @super-popup-height 92%
run-shell '/home/vincent/projects/bone/tmux-super-popup/super-popup.tmux'
```
