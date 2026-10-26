# Customizing bone

bone is built to be customized from Lua without touching its source. This guide
is the entry point; each linked guide is short and self-contained — read only
the one that fits the task, then make the change.

bone writes these guides to `~/.bone/docs/` at startup, so they always match
the installed version; the paths below are relative to that folder. Lua code
(in the core and each frontend, in tools, hooks and UI code) gets them from the `bone.docs` API,
by the same paths:

```lua
bone.docs.list()                       -- { { name = "customizing/customizing-tui.md", desc = "..." }, ... }
local guide = bone.docs.read("customizing/customizing-tui.md")
```

## Where customizations live

| File | Runs in | What it controls |
|---|---|---|
| `~/.bone/core.lua` | the core (server) | providers, system prompt, tools, hooks |
| `~/.bone/tui.lua` | the TUI | options, keys, commands, colors, panels, chat look |
| `~/.bone/ui.lua` | the TUI and the GUI | UI config for every frontend; runs before `tui.lua` / `gui.lua` |
| `~/.bone/gui.lua` | the GUI (`bone --gui`) | the GUI's own config: `bone.gui.window`, `bone.o.font` (see `customizing-gui.md`) |
| `~/.bone/plugins/<name>/` | core and frontends | optional `core.lua`, `ui.lua` (every frontend), `tui.lua`, `gui.lua`, and `lua/` modules |
| `.bone/` in a trusted project | each frontend | `ui.lua`, then that frontend's file (`tui.lua` or `gui.lua`); `/plugins trust` once covers both |
| `~/.bone/docs/` | — | these guides, rewritten at startup (edits there are lost) |

The runtime defaults (keymaps, the standard look, the `/` menu) live in
`runtime/`, built into the binary. A frontend resolves a file from its active
library first (`~/.bone/libraries/<name>/`, chosen with `/tui` or `/gui`;
which one is active is in `customizing-tui.md` and `customizing-gui.md`),
then `~/.bone/runtime/<same path>`, then the built-in copy. Prefer changing
pieces from your config:
`local M = bone.builtin("bone.ui.layout")` gives you the built-in module to
wrap. `bone.builtin` always reads that built-in copy.

The full API reference is `lua.md` (next to this guide; also `/help lua` in
the TUI), and `architecture.md` explains how the core, server and frontends
fit together. A guide below names exactly which API calls it uses, so you only
need to open `lua.md` for a call a guide does not show.

## Guides

- **`customizing/customizing-tui.md`** — change how the TUI looks and behaves: options, keys,
  slash commands, colors, regions, the prompt box, the statusline.
- **`customizing/customizing-gui.md`** — customize the desktop GUI: node trees,
  styles, slots, libraries (`/gui`), fonts, inputs, panels and the window.
- **`customizing/customizing-panels.md`** — add a panel docked beside the chat (a file list,
  todo list, job viewer).
- **`customizing/customizing-popups.md`** — popups, pickers, pagers and notifications for
  interactive features.
- **`customizing/customizing-chat.md`** — how each item in the chat looks: views, tool call
  rendering, custom items, click handling.
- **`customizing/customizing-core.md`** — new tools, hooks, providers, custom system prompts,
  sub-agent sessions (core side).
- **`customizing/customizing-plugins.md`** — packaging any of the above as a plugin in
  `~/.bone/plugins/`, with state, settings and lifecycle.

## Rules of thumb

- Lua has full trust and is never sandboxed; a Lua error is reported to the
  user and does not crash bone.
- The frontends do not share a drawing model. In the TUI, Lua supplies
  lines, each a list of items: a plain string, `{ "text", "HighlightGroup" }`,
  or `{ fill = "─", hl = "Group" }` (stretches to fill the row). Rust keeps
  the data, wrapping, scrolling and mouse positions. In the GUI, Lua supplies
  a tree of nodes (`bone.gui.view`); see `customizing/customizing-gui.md`. A
  window that still returns lines is drawn in a monospace font.
- Highlight groups are named (`Accent`, `Dim`, `ToolPath`, `StatusLine`, …;
  `bone.hl.names()` lists them). Color them with `bone.hl.set` or a
  colorscheme (`bone.colorscheme("ansi")`).
- Reloading is per frontend, and not the same in both. See
  `customizing/customizing-tui.md` and `customizing/customizing-gui.md`.
  Check `bone.api.log(50)` for Lua errors.
