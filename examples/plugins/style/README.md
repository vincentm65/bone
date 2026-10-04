# style

A complete look for bone, built only from the Lua API, drawn over the
runtime's standard UI.

```sh
cp -r examples/plugins/style ~/.bone/plugins/
```

- `lua/style/views.lua`: how each chat item looks (your messages, reasoning,
  Markdown answers, condensed tool calls with previews and diffs, notices).
- `lua/style/ui.lua`: the statusline and the divider above the prompt.
- `tui.lua`: loads both, sets the prompt prefix and placeholder, and shows a
  hint in an empty session.

Change anything from `~/.bone/tui.lua` (it runs after plugins), or copy the
files and edit them.
