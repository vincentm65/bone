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

## Where a change goes

**Changes to an app (how the TUI or GUI looks or behaves) go in a UI library
of the user's own:** a folder in `~/.bone/libraries/NAME/`. Nothing else.

- The built-in libraries, **standard** and **bare**, are read from the binary
  and cannot be changed. Never edit bone's source or try to replace a
  built-in file; never copy one whole into a library.
- A library holds only its changes. Its entry file runs the built-in UI first,
  so everything it does not change keeps getting bone's updates:

  | App | Entry file in the library | Which library the app uses |
  |---|---|---|
  | terminal (TUI) | `tui/defaults.lua` | `~/.bone/library` (just the name; none = standard) |
  | desktop (GUI) | `gui/defaults.lua` | `~/.bone/gui-library`; without it, the TUI's choice if that library has a `gui/` folder, else standard |

  A new library's entry file is exactly one line, the built-in UI it starts
  from: `bone.builtin("tui/defaults.lua")` (or `"gui/defaults.lua"`; bare is
  `"tui/bare.lua"` / `"gui/bare.lua"`). Write the library's name to the
  app's file above to switch to it; the app reloads by itself.
- Not UI: tools, hooks, providers and the system prompt go in
  `~/.bone/core.lua` (`customizing/customizing-core.md`). Something to share
  with others is a plugin (`customizing/customizing-plugins.md`).

## How to make a change in a library

1. **Most changes are a few lines in the entry file, after the
   `bone.builtin(...)` line**: options (`bone.o`), keys (`bone.keymap.set`),
   slash commands (`bone.cmd.create`), colors (`bone.hl.set`,
   `bone.colorscheme`), the statusline, regions, chat views, panels. The
   guides' snippets, written for `tui.lua` and `gui.lua`, work there too.
2. **To change part of a built-in module**, give the library a file at the
   module's path that starts from the built-in one and changes only what it
   needs. The app loads the library's file in place of the built-in module:

   ```lua
   -- ~/.bone/libraries/NAME/lua/bone/ui/layout.lua
   local M = bone.builtin("bone.ui.layout")   -- the built-in module
   local setup = M.setup
   function M.setup(opts)
     setup(opts)
     bone.ui.regions.top = function() return { "my header" } end
   end
   return M
   ```

   To see what a built-in module does, read its copy in
   `~/.bone/docs/builtin/` (same path as in the map below). Those copies are
   for reading only: bone rewrites them at every start and never loads them.
3. Save. The app reloads; a Lua error is shown to the user and listed by
   `/health`, and the app keeps running.

## Which built-in file does what

Read them in `~/.bone/docs/builtin/`; a library file at the same path
replaces one.

| Area | TUI | GUI |
|---|---|---|
| Entry (what loads, default keys) | `tui/defaults.lua` | `gui/defaults.lua` |
| Chat items (messages, tool calls, diffs) | `lua/bone/ui/views.lua` | `lua/bone/desktop/views.lua`, `lua/bone/desktop/tools.lua` |
| Layout (regions, prompt box, empty chat) | `lua/bone/ui/layout.lua`, `lua/bone/ui.lua` | `lua/bone/nodes/library.lua`, `lua/bone/nodes/window.lua` |
| Statusline / status bar | `lua/bone/ui/statusline.lua` | `lua/bone/nodes/statusbar.lua` |
| Prompt / composer | `lua/bone/ui/layout.lua` | `lua/bone/nodes/composer.lua` |
| Sessions list | `lua/bone/ui/sessions.lua` | `lua/bone/desktop/sidebar.lua` |
| What a chat needs from you (a question open, a reply not seen), as data | `lua/bone/chats.lua` | same; drawn by `widgets.status_dot` in `lua/bone/nodes/widgets.lua` |
| Queue, sub-agents, jobs under the prompt | `lua/bone/ui/tray.lua` | `lua/bone/desktop/tray.lua` |
| Sizes, corners, spacing | — | `lua/bone/nodes/style.lua` |
| Colors | `colors/black.lua`, `colors/ansi.lua` | `colors/graphite.lua` |
| Key bindings (both apps) | `lua/bone/keys.lua` | same |
| Slash commands and the `/` menu | `lua/bone/commands.lua`, `lua/bone/menu.lua` | same, plus `lua/bone/gui.lua` |
| Pickers, pagers, popups | (built into the TUI) | `lua/bone/desktop/pick.lua`, `lua/bone/nodes/popups.lua` |
| Settings, setup, help | `lua/bone/config.lua`, `lua/bone/setup.lua`, `lua/bone/help.lua` | `lua/bone/desktop/settings.lua`, `lua/bone/desktop/setup.lua`, `lua/bone/desktop/help.lua` |

## Other files bone loads

These still run, after the library, but changes the agent makes go in the
library:

| File | Runs in | Use |
|---|---|---|
| `~/.bone/ui.lua`, `~/.bone/tui.lua`, `~/.bone/gui.lua` | the TUI and GUI; the TUI; the GUI | personal config written by hand (skipped by bare) |
| `~/.bone/plugins/<name>/` | core and frontends | installed plugins: `core.lua`, `ui.lua`, `tui.lua`, `gui.lua`, `lua/` |
| `.bone/` in a trusted project | each frontend | `ui.lua`, then `tui.lua` or `gui.lua`; `/plugins trust` once covers both |
| `~/.bone/docs/` | — | these guides, rewritten at startup (edits there are lost) |

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
