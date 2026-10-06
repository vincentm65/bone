# Packaging a customization as a plugin

A plugin is a folder in `~/.bone/plugins/`. Use one when a customization has
more than a few lines, belongs to a feature (a provider, a task manager, a
review flow), or should be installable on other machines. Everything in the
other guides works in a plugin's files unchanged.

```text
~/.bone/plugins/git/
  core.lua     runs in the core (tools, hooks)
  tui.lua      runs in the TUI (keys, commands, tool views)
  lua/         modules for require()
  colors/      colorschemes
  manifest.json  optional: a settings page in /config
```

Every part is optional. Plugins load in name order, after the runtime
defaults and before your own `core.lua` / `tui.lua`, so your config can change
anything a plugin set up. Rename a folder to start with `_` or `.` to disable
it. `bone.plugins` lists the loaded names. Installing is just copying or
`git clone`-ing into `~/.bone/plugins/`.

The repo's `examples/plugins/` show most of the API at work: `style` (a
complete look), `approve` (asking before tools run), `git` and `anthropic`
(tools and a provider), `switch` (pick the provider from the TUI), `tasks`
(a persistent task panel), `review`, `stats`, `testrun`, `output-cap`,
`retry`, `mcp`, `skills`, `templates`, `ask-model`. None is installed by
default. Read the one closest to what you are building.

## State

```lua
-- in plugins/todo/tui.lua
local st = bone.plugin.state()            -- ~/.bone/state/tui/todo.json, saved on unload and quit
st.items = st.items or {}
bone.plugin.on_shutdown(function() ... end)  -- unload, reload and quit
```

`bone.state.load(name)` / `bone.state.save(name, value)` are the raw form
(JSON in `~/.bone/state/<side>/<name>.json`); `bone.plugin.state(name)` wraps
them with automatic saving. Both sides have it; each side has its own files.
With `{ shared = true }` as the last argument both sides use
`~/.bone/state/shared/<name>.json`, which is how a plugin's core and TUI
halves share settings.

A plugin owns what it creates while its file or one of its callbacks runs:
keymaps, user commands, `bone.on` handlers, panels, windows, jobs, dynamic
options, raw key interceptors and new keymap contexts. Unloading it removes
all of that (a key or command the user has since redefined is left alone),
cancels its running jobs without calling back, and forgets the modules it
`require`d, so loading it again runs fresh code. Anything else it changed
(views, highlights, `bone.ui` fields) is for its shutdown hook to undo.
Panels do not carry over a reload: on `ready` with `reload = true`,
`ev.panels` lists the ids that were open, so reopen yours.

`bone.plugin.current()` is `{ name, dir, kind }` while the plugin's file runs
(or in the TUI, while any of its callbacks run), else nil.

Loading at runtime: `bone.plugin.load(name)`, `bone.plugin.unload(name)`,
`bone.plugin.reload(name)`; also `/plugin load|unload|reload name`. A plugin
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

or from `tui.lua` with `bone.settings.page{ name = "myplugin", title =
"My plugin", fields = { … } }` (same field shape). Values are saved as
`<name>.<key>` in `settings.json` and read with `bone.settings.get` (falling
back to the default). `require("bone.config").open(tab)` opens the page on a
tab.

## Two halves

The common shape of a plugin is a core half (a tool, or hooks) and a TUI half
(showing what the core does): they talk through the protocol.

- The core exposes a function to the TUI with `bone.rpc.register(name, fn)`;
  the TUI calls it with `bone.rpc.call(name, args, cb)` (for the session on
  screen).
- The core emits questions and events; the TUI reacts with `bone.on` and
  answers with `bone.request(method, params, cb)`.

`examples/plugins/approve/` is the reference: its `core.lua` asks
`bone.ask({ kind = "approval", … })` before risky tools, its `tui.lua` shows
each question as a popup (`y`/`a`/`n`) and answers with
`bone.request("ask/respond", …)`.

## Project config

A project can carry TUI config in `.bone/tui.lua` (and modules in
`.bone/lua/`). bone looks for it in the working directory and its parents,
but Lua has full trust, so it only runs after you trust that directory:
`/plugin trust` runs it now and on later starts there, `/plugin untrust`
unloads it and forgets the trust, and `/plugin` lists it (as `project`). It
loads after your own `tui.lua`, as a plugin named `project`, so it can be
unloaded like any other. `bone.project.info()` returns `{ root, file,
trusted, loaded }` or nil.
