# Packaging a customization as a plugin

A plugin is a folder in `~/.bone/plugins/`. Use one when a customization has
more than a few lines, belongs to a feature (a provider, a task manager, a
review flow), or should be installable on other machines. Everything in the
other guides works in a plugin's files unchanged.

```text
~/.bone/plugins/myplugin/
  core.lua     runs in the core (tools, hooks)
  ui.lua       runs in every frontend (the TUI and the GUI), first
  tui.lua      runs in the TUI only, after ui.lua
  gui.lua      runs in the GUI only (bone --gui), after ui.lua
  lua/         modules for require()
  colors/      colorschemes
  manifest.json  optional: a settings page in /config
```

Every part is optional. A `README.md` is the plugin's help page while it
is loaded: `/help <name>` opens it and the help browser lists it under Docs.
Give its keys a `desc` (`bone.keymap.set(key, fn, { desc = "…" })`) and they
are listed under Keys. Plugins load in name order, after the runtime
defaults and before your own `core.lua`, `ui.lua`, `tui.lua` and `gui.lua`,
so your config can change anything a plugin set up. Rename a folder to start
with `_` or `.` to disable it. `bone.plugins` lists the loaded names. Installing is just copying or
`git clone`-ing into `~/.bone/plugins/`.

The [Bone catalog](https://github.com/vincentm65/bone-catalog) is the source of installable packages. Use `/catalog` to install and update them, or read packages such as `mcp`, `skill`, `usage`, `themes` and `review` for API usage. None is installed by default.

## The TUI and the GUI

The TUI and the desktop GUI run the same Lua UI API, with one
implementation, so most UI code belongs in `ui.lua` and works in both:
keymaps, commands, options, events, colors, chat data, windows
(`bone.ui.popup`) and `bone.request`. How the window looks is not shared:
the TUI draws character cells (layout, regions, panels, chat views as
`lines`) and the GUI draws a tree of nodes (`bone.gui.view`). A window's
`lines` are drawn in both, in a monospace font. The GUI still emits the
shared `mouse` event over those rows, but they are not pointer targets:
`on_click` and the other node handlers exist only when the window has
`node = fn`.

### One view for both: elements

A window or panel given `view` in place of `lines`, `render` and `node` is
drawn by each frontend's library from the same data. The view returns
elements, which say what something is, never how it looks: no colors, sizes
or spaces, only a `tone` (`normal`, `dim`, `accent`, `success`, `warning`,
`error`) and states. The terminal draws them as lines and the desktop app
with its widgets, each in the look of the window and the colorscheme.

```lua
local done = false
bone.ui.panel.open({ id = "steps", dock = "right", title = "Steps", view = function()
  return { kind = "column",
    { kind = "checklist", items = { { text = "build", state = done and "done" or "active" } } },
    { kind = "button", label = "Mark done", on_press = function() done = true end },
  }
end })
```

The kinds are `text`, `row`, `column`, `section`, `split` (a narrow column
beside the rest), `list`, `checklist`, `button`, `tabs`, `field`, `diff`,
`code`, `markdown`, `badge`, `meter`, `spinner`, `divider` and `hints`;
`runtime/lua/bone/elements.lua` lists their fields. What has an `on_press`
(and each tab) is a target: a click presses it, in the terminal too, and in
a focused window or panel up/down (or `k`/`j`) move a cursor through the
targets, left/right change tabs, and enter or space press. Up from the first
target and down from the last scroll the panel or window, and so do the page
keys; a window taller than the screen scrolls in the terminal too. While a
`field` is `editing`, every key is the plugin's.

A list with `selected` and `on_select` keeps a selection its plugin owns (a
picker, the left side of a `split`): the cursor follows it, moving selects,
a click selects and a click on the selected entry presses it. Its entries
take `depth` (a tree) and `badges`, and `on_key` on a list, field or button
gets the keys the cursor's target does not use. `bone.ui.pager` takes an
element too, for a long page that scrolls. A view runs each time the frontend draws (every frame in the
terminal); returning the same table as last time reuses what was drawn, so a
large view can build its tree only when it changes. `bone.has_capability("ui.elements")`
says the frontend has them.

Use `lines`/`render` and `node` only for what elements cannot say (a
terminal, a canvas, a layout of your own), and give both frontends one. Even
then, color with the colorscheme's groups, never hex. A group of your own is
`bone.hl.set("MyDone", { link = "Success", default = true })`: under the
colorscheme, so a scheme can restyle it, and linked, so it follows a scheme
change.

Put what only one frontend has in its own file, or check for it:

```lua
-- in ui.lua: shared, with a GUI-only extra
bone.cmd.create("todo", open_todo, { desc = "the todo panel" })
if bone.has_capability("gui.notifications") then
  bone.gui.notify("Todo", "ready")
end
```

`bone.api_info().side` is `"tui"` or `"gui"`, and `bone.has_capability`
answers for this frontend: the shared API is `ui.*` (`ui.keymaps`,
`ui.panels`, …), the GUI's own is `gui.*` (`gui.window`, `gui.notifications`), and
the TUI also reports its older `tui.*` names. In the GUI, a call to an API
it does not have yet raises an error naming it; `docs/lua.md` lists what is
shared.

`bone.plugin.list()` says what each plugin has: `core` (a `core.lua`) and
`frontends` (`{ "tui", "gui" }` for a `ui.lua`, or the ones with their own
file). The catalog shows the same (`both`, `tui`, `gui`, or `core`), from a
package's files or an explicit `"frontends": ["tui", "gui"]` in its
`catalog.json` entry.

## State

```lua
-- in plugins/todo/tui.lua  (gui.lua writes state/gui/, core.lua state/core/)
local st = bone.plugin.state()            -- ~/.bone/state/tui/todo.json
st.items = st.items or {}
bone.plugin.on_shutdown(function() ... end)  -- explicit unload, reload and quit
```

`bone.state.load(name)` / `bone.state.save(name, value)` are the raw form:
JSON at `~/.bone/state/core/`, `state/tui/` or `state/gui/` (`<name>.json`).
`bone.plugin.state(name)` wraps that file. Each of the core, the TUI and the
GUI has its own. With `{ shared = true }` as the last argument every side
uses `~/.bone/state/shared/<name>.json`, which is how a plugin's core and
frontend halves share settings.

A plugin owns what it creates while its file runs, or while any of its
callbacks run, in either frontend: keymaps, user commands, `bone.on`
handlers, panels, windows, jobs, dynamic options, raw key interceptors and
new keymap contexts. An explicit unload, reload or quit (closing the TUI, or
the GUI window) is that lifecycle: `on_shutdown` runs, `bone.plugin.state` is
saved, and what it owns is removed (a key or command the user has since
redefined is left alone). Its running jobs are cancelled without calling
back, and the modules it `require`d are forgotten, so loading it again runs
fresh code. Anything else it changed (views, highlights, `bone.ui` fields) is
for its shutdown hook to undo.

A watched-file reload is not that unload. On the TUI it does save state and
run shutdown hooks, and panels do not carry over: `ready` is
`{ reload = true, panels }`, the ids that were open, so reopen yours. That
payload is TUI only. The GUI's file reload does not run shutdown hooks, does
not save state, and fires `ready` with no payload; the hooks and the save run
when the window closes. Save important state as it changes with
`bone.plugin.save_state()` rather than relying on a GUI file reload.

`bone.plugin.current()` is `{ name, dir, kind }` while the plugin's file or
one of its callbacks runs, in either frontend, else nil.

Loading at runtime: `bone.plugin.load(name)`, `bone.plugin.unload(name)`,
`bone.plugin.reload(name)`; also `/plugins load|unload|reload name`. A plugin
cannot unload itself from its own code. `bone.plugin.list()` gives
`{ name, dir, kind, loaded, error }` for each plugin seen this session.

## Settings

Declare a settings page in `manifest.json` (works for core-only plugins too):

```json
{ "title": "Web search", "settings": [
  { "key": "num_results", "label": "Results per search", "type": "integer",
    "default": 5, "min": 1, "max": 10,
    "desc": "when the model does not ask for a number" } ] }
```

or from `ui.lua`, `tui.lua` or `gui.lua` with `bone.settings.page{ name = "myplugin", title =
"My plugin", fields = { … } }` (same field shape). Values are saved as
`<name>.<key>` in `settings.json` and read with `bone.settings.get` (falling
back to the default). `require("bone.config").open(tab)` opens the page on a
tab.

## Two halves

The common shape of a plugin is a core half (a tool, or hooks) and a
frontend half (the TUI, the GUI, or both, showing what the core does): they
talk through the protocol.

- The core exposes a function with `bone.rpc.register(name, fn)`; a frontend
  calls it with `bone.rpc.call(name, args, cb)` (for the session on screen).
- The core emits questions and events; the frontend reacts with `bone.on` and
  answers with `bone.request(method, params, cb)`.

For confirmation, a core hook calls `bone.ask({ kind = "confirm", text = "Proceed?" })`. A frontend handler listens for `ask/requested` and replies with `ask/respond`; see the minimal paired examples in [lua.md](../lua.md#asking-the-user) and the popup guide.

## Project config

A project can carry UI config in `.bone/ui.lua`, `.bone/tui.lua` or
`.bone/gui.lua` (and modules in `.bone/lua/`), each run by the frontends
it is for. bone looks for it in the working directory and its parents,
but Lua has full trust, so it only runs after you trust that directory:
`/plugins trust` runs it now and on later starts there, `/plugins untrust`
unloads it and forgets the trust, and `/plugins list` lists it (as `project`). Trust is
kept once for both frontends. It loads after your own config, as a plugin named `project`, so it can be
unloaded like any other. `bone.project.info()` returns `{ root, file,
trusted, loaded }` or nil.
