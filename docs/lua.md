# Lua in bone

bone runs two separate LuaJIT states, one per side:

| File | Runs in | Controls |
|---|---|---|
| `~/.bone/core.lua` | the core (server) | providers, system prompt, tools, hooks |
| `~/.bone/tui.lua` | the TUI | options, keys, commands, colors, events |

`$BONE_CONFIG_DIR` replaces `~/.bone`. The two states never share Lua values; they talk only through the bone protocol. Lua has full trust (no sandbox). Tool calls run without asking; asking first is a customization using `bone.ask` (see below).

Numbering, everywhere: indexes into lists start at 1, as in Lua (chat item `index`, `first`/`last`, `from`/`to`); positions start at 0 (screen cells `x`, `y`; prompt `row`, `col` and highlight ranges). Settings are fields you assign (`bone.ui.statusline = fn`, `bone.ui.layout = {…}`, `bone.ui.spinner = {…}`, `bone.o.name = value`); actions are calls (`bone.chat.add`, `bone.ui.popup`, `bone.notify`).

Load order on each side: `runtime/<side>/api.lua`, then `runtime/<side>/defaults.lua`, then your file. The runtime is built into the binary, and any runtime file can be replaced by putting a file at the same path under `~/.bone/runtime/` (for example `~/.bone/runtime/tui/defaults.lua` drops every default keymap). A replacement hides every later change to the built-in file, so prefer changing pieces from your config; when you do replace a file, start from the built-in one and change only what you need:

```lua
-- ~/.bone/runtime/lua/bone/ui/layout.lua
local M = bone.builtin("bone.ui.layout")   -- the built-in module, not this file
local setup = M.setup
function M.setup(opts)
  setup(opts)
  bone.ui.regions.top = function() return { "my header" } end
end
return M
```

`bone.builtin(name)` runs the built-in version of a module (`"bone.ui.layout"`) or file (`"tui/defaults.lua"`) and returns what it returns. Bone never writes into `~/.bone/runtime/`, so your copies are safe from updates. `/runtime` lists them (and which are still identical to the built-in), and `/runtime reset FILE` or `/runtime reset all` goes back to the built-in versions: your copies are moved to `~/.bone/runtime-backup/<time>/`, not deleted, and the TUI reloads by itself. The TUI mentions overrides at startup and `/health` lists them.

`require("x.y")` finds `~/.bone/lua/x/y.lua` (or `x/y/init.lua`), then each plugin's `lua/`, then the runtime's `lua/` modules.

## Extension contract (API v1)

`bone.version` is the binary/package version. It is not an extension
compatibility check. Every Lua state also reports the version of the Lua
contract and the features implemented by that state:

```lua
bone.api_version                  -- integer; currently 1
bone.capabilities["tui.keymaps"]  -- true when this feature exists
bone.has_capability("tui.panels") -- false for an unknown/unavailable feature

local info = bone.api_info()
-- { version = 1, side = "tui", capabilities = { ... } }
```

`bone.api_version` changes only for a breaking change to an existing API
meaning or call shape. New functions, fields, events and capabilities are
additive. Plugins should test `bone.has_capability(name)` before using a
newer extension point rather than comparing `bone.version`. The core and TUI
states are separate; a capability in one state does not make it available in
the other.

The current capability names include `core.config`, `core.tools`,
`core.hooks`, `core.providers`, `core.jobs`, `core.http_stream`, `core.ask`,
`core.health`, `core.ready`, `core.reload`, `core.hooks.system`, `core.hooks.context`, `core.hooks.errors`, `core.hooks.stream`, `core.hooks.session`, `core.session_write`, `core.queue`, `core.model`, `core.mcp`, `core.rpc`, `plugins.state` (both sides), and, in the TUI, `plugins.lifecycle`, `tui.project`, `tui.model`, `tui.rpc`, `tui.keymaps`, `tui.input`, `tui.events`,
`tui.local_events`, `tui.commands`, `tui.command_specs`, `tui.options`,
`tui.dynamic_options`, `tui.request`, `tui.prompt`, `tui.prompt_edit`, `tui.chat`, `tui.chat_data`, `tui.windows`,
`tui.panels`, `tui.regions`, `tui.views`, `tui.pickers`, `tui.themes`, `tui.jobs`, `jobs.streaming` and
`tui.session`. Both sides also report `lua`, `json`, `modules` and `fs`.
Capability names describe the contract, not the internal Rust module layout.

### Ownership and compatibility

The core remains the authority for sessions, turns, providers, tools and
protocol state. TUI Lua can render, handle input, and make protocol requests,
but it cannot become a second session authority. A Lua error is reported to
the user and does not crash the process.

The following v1 entry points are compatibility APIs and will keep working as
newer, more structured APIs are added around them:

- `bone.keymap.set` / `bone.keymap.del`
- callable `bone.cmd`, `bone.cmd.create` / `bone.cmd.del`
- `bone.o` option reads and writes
- `bone.on` / `bone.off`
- `bone.ui.win`, including its `bone.ui.popup` and update/close helpers

Their current argument and return behavior is documented below. New APIs must
not silently change an existing wrapper's meaning; a wrapper may delegate to
the newer implementation internally.

### TUI extension points (API v1)
The following table records the compatibility boundary and the additive
capabilities currently implemented. The last column names work that remains
planned; those capabilities are not available until reported by
`bone.has_capability`.
| Extension point | Compatibility behavior | Current additive extension / planned work |
|---|---|---|
| Input contexts | `main` is the prompt context and `popup` is the fallback context for a focused Lua window. A picker is a Lua window, not a third keymap context. Unmapped main text enters the prompt. | Named contexts, priority/fallback, key sequences, raw interception and real sequence timeouts are available through `tui.input`. |
| Events | `bone.on` receives server notifications by method plus `ready` and `submit`. Server callbacks run after the TUI applies the notification; `submit` can cancel or replace text. | Local prompt/focus/resize/key/paste and panel lifecycle events are available through `tui.local_events`. |
| Commands | Built-ins and user commands are slash commands. `bone.cmd.create` accepts one raw `{ args = "..." }` string and `{ desc }`; built-ins cannot be replaced. | Canonical names, aliases, alias-aware completion/execution/deletion, completion callbacks and typed argument metadata are available through `tui.command_specs`. |
| Options | `bone.o` exposes the fixed, typed Rust options. v1 values are booleans or non-negative numbers; unknown names and wrong types are errors. | Dynamic boolean, integer, number and string options, defaults, metadata, deletion and change callbacks are available through `tui.dynamic_options`. |
| Panels | `bone.ui.win` is a Lua-drawn overlay, and regions/views customize the existing chat layout. Both keep working unchanged. | Persistent panels with IDs, docking, sizing, scrolling, focus, render/key callbacks and lifecycle events are available through `tui.panels` (`bone.ui.panel`). |
| Prompt and chat | `bone.api.prompt_get`/`prompt_set` read and replace the whole prompt; `bone.chat.items({ kind, last })` lists the on-screen chat's items. | Cursor, range and selection edits (`tui.prompt_edit`, `bone.prompt`) and structured read-only items, turns and sessions with filters (`tui.chat_data`) are available. The TUI stays a prompt, not a file editor. |
| Jobs | TUI `bone.system`, `bone.http` and `bone.defer` run one background operation and invoke a callback. Core waits are coroutine-based and turn cancellation stops them. v1 has no general process handle or streamed TUI job. | Streaming jobs with handles, stdout/stderr callbacks (chunks or lines), stdin writes, cancellation of the whole process group, timeouts, exit results, status and `job/*` events are available in the TUI through `jobs.streaming` (`bone.job`). |
| Plugin state | Folder plugins load in name order between runtime defaults and the user's config; `bone.plugins` reports names and `.`/`_` folders are disabled. There is no persistent state store, reload hook or automatic resource cleanup yet. | In the TUI, plugins own what they create; `bone.plugin.on_shutdown`, load/unload/reload (`/plugins`), auto-saved `bone.plugin.state` and trusted project config (`/plugins trust`) are available through `plugins.lifecycle` and `tui.project`. Both sides have `bone.state` and `bone.plugin.current()` (`plugins.state`). |

The existing detailed sections describe both the compatibility calls and the
new additive APIs. A plugin may use a compatibility API today and opt into
each later capability independently.

## Plugins

A plugin is a folder in `~/.bone/plugins/`:

```text
~/.bone/plugins/myplugin/
  core.lua     runs in the core (tools, hooks)
  tui.lua      runs in the TUI (keys, commands, tool views)
  lua/         modules for require()
  colors/      colorschemes
```

The [Bone catalog](https://github.com/vincentm65/bone-catalog) is the source of installable plugins. Use `/catalog` for packages such as `mcp`, `skill`, `usage`, `themes` and `review`; none is installed by default.

Every part is optional. Plugins load in name order, after the runtime defaults and before your own `core.lua` / `tui.lua`, so your config can change anything a plugin set up. Rename a folder to start with `_` or `.` to disable it. `bone.plugins` lists the loaded names. Install published packages with `/catalog`. Your own plugins can be placed directly in `~/.bone/plugins/`.

### Plugin lifecycle and state

`bone.plugin.current()` is `{ name, dir, kind }` while a plugin's file runs (on both sides; in the TUI also while any of its callbacks run), else nil.

`bone.state.load(name)` returns the table saved under `name` (empty if none) and `bone.state.save(name, value)` saves one, as JSON in `~/.bone/state/<side>/<name>.json`. Names are letters, digits, `_`, `-` and `.`. Both sides have it; each side has its own files. With `{ shared = true }` as the last argument both sides use `~/.bone/state/shared/<name>.json`, which is how a plugin's core and TUI halves can share settings (a save replaces the file atomically; the last writer wins).

In the TUI a plugin owns what it creates while its `tui.lua` or one of its callbacks runs: keymaps, user commands, `bone.on` handlers, panels, windows, jobs, dynamic options, raw key interceptors and new keymap contexts. Unloading it removes all of that (a key or command the user has since redefined is left alone), cancels its running jobs without calling back, and forgets the modules it `require`d, so loading it again runs fresh code. Anything else it changed (views, highlights, `bone.ui` fields) is for its shutdown hook to undo.

```lua
-- in plugins/todo/tui.lua
local st = bone.plugin.state()            -- ~/.bone/state/tui/todo.json, saved on unload and quit
st.items = st.items or {}
bone.plugin.on_shutdown(function() ... end)  -- unload, reload and quit
```

- `bone.plugin.list()`: `{ name, dir, kind, loaded, error }` for each plugin seen this session.
- `bone.plugin.load(name)` (a folder in `plugins/`, even one added after startup or disabled with `_`), `bone.plugin.unload(name)`, `bone.plugin.reload(name)`; also `/plugins load|unload|reload name`. A plugin cannot unload itself from its own code.
- `bone.plugin.state(name)` returns a table that is loaded once and saved when its plugin unloads and when bone quits; `bone.plugin.save_state(name)` saves it now. `name` defaults to the current plugin.
- Events: `plugin/loaded` and `plugin/unloaded` with the plugin's info.

### Project config

A project can carry TUI config in `.bone/tui.lua` (and modules in `.bone/lua/`). bone looks for it in the working directory and its parents, but Lua has full trust, so it only runs after you trust that directory: `/plugins trust` runs it now and on later starts there, `/plugins untrust` unloads it and forgets the trust, and `/plugins list` lists it (as `project`). Until then bone says that there is one. It loads after your own `tui.lua`, as a plugin named `project`, so it can be unloaded like any other. `bone.project.info()` returns `{ root, file, trusted, loaded }` or nil.

## Shared

- `bone.side`: `"core"` or `"tui"`. `bone.version`, `bone.config_dir`.
- `bone.json.encode(value)`, `bone.json.decode(string)`
- `bone.inspect(value)`: readable dump of any value.
- `bone.util`: `split(s, sep)`, `trim(s)`, `startswith(s, prefix)`, `extend(t1, t2, ...)`, `front_matter(text)` → `meta, body` for Markdown files that start with `---` / `key: value` lines / `---`.
- `bone.fs.list(dir)` → `{ { name, type = "file" | "dir" } }`, sorted (`~/` works), or `nil` and an error.
- `bone.docs.list()` → `{ { name, desc } }`, the customization guides (see `docs/customizing.md`); `bone.docs.read(name)` → the guide's text, or `nil` and an error. `name` is the guide's path, such as `customizing/customizing-tui.md`. bone also writes the guides to `~/.bone/docs/` at startup, and the agent's system prompt points it there.

### Settings

`~/.bone/settings.json` holds the choices made in the app, as opposed to `core.lua` and `tui.lua`, which are yours and which bone never writes. The core owns the file: it checks each change, writes the file whole, and tells every client (`settings/changed`), so several TUIs stay in step. Delete the file to go back to the defaults; if it is not valid JSON, bone ignores it, says so, and refuses to change it until you fix it.

```json
{
  "provider": "qwen",
  "providers": { "qwen": { "model": "Qwen3.8-27B-exl3-3.8bpw" },
                 "work": { "base_url": "https://api.example.com/v1", "model": "big", "api_key_env": "WORK_KEY" } },
  "tui": { "tool_detail": "rows", "show_reasoning": true },
  "web_search": { "num_results": 5 }
}
```

- `provider` picks which provider turns use, over `core.lua`'s own `bone.config.provider`. `providers.<name>.<field>` changes one field of a provider (`model`, `base_url`, `type`, `reasoning_effort`, `stream_usage`, `api_key_env`), over what `core.lua` gives it; `providers.<name>` as a whole (`{ base_url, model, … }`) adds a provider that `core.lua` does not have, as `/setup` and `/config` do. So `core.lua` defines providers and their defaults, settings change and add them, and `BONE_*` environment variables override both for one run. Changing any of this reloads the core's configuration at once, and is undone if that fails (an unknown provider is refused). A provider's key is the one saved in `~/.bone/secrets.json` (`secrets/set`; readable only by you, never sent back to clients, never in `settings.json`), else its `api_key_env` variable, else `core.lua`'s. Adding a provider while `settings.json` names none also chooses it. With no provider at all the core still starts, and turns say to run `/setup`; with several and none chosen it asks you to choose.
- `tui.<option>` sets a TUI option after the defaults and plugins, before your `tui.lua`, which runs last and wins. `ctrl+t` and `ctrl+r` save their choice here.
- `providers.<name>` defines a provider (`{ base_url, model, type, api_key_env }`), as `/setup` does; it goes into `bone.config.providers` before plugins and `core.lua`, which may replace it. Its key is the one saved in `~/.bone/secrets.json` (`secrets/set`; readable only by you, never sent back to clients, never in `settings.json`), else the `api_key_env` variable. Adding a provider while `settings.json` names none also chooses it. With no provider at all the core still starts, and turns say to run `/setup`; with several and none chosen it asks you to choose.
- `catalog.url` is where `/catalog` gets packages (below).
- Anything else is free for plugins: a plugin keeps its settings under its own name (`web_search.num_results`).

`/setup` adds a provider in three steps: what kind (a server on this machine, OpenAI, DeepSeek, OpenRouter, or any OpenAI-compatible service), its name, URL, model and key (typed hidden, saved in `secrets.json`, or the variable that holds it), and optionally packages from the catalog. It opens by itself when bone starts with no provider; dismissing it is remembered (`setup.skipped`), and `/setup` brings it back. It never writes `core.lua`.

`/catalog` lists the catalog's packages as Updates (an installed file differs), Installed and Available. Use space to check packages, `a` to check every available/update, `n` to clear checks, and Enter to install the selection in one pass. `u` scans installed files for updates, `r` refreshes the index, and `x` removes a package (moved to `~/.bone/plugins-backup/`); install and update operations ask first because a package's Lua runs with your permissions. Packages come from `catalog.url`: a URL (by default `https://raw.githubusercontent.com/vincentm65/bone-catalog/refs/heads/main`) or a local folder (`~/projects/bone-catalog`), holding `catalog.json` and `plugins/<name>/…` as the repository's root does. Every file is checked against the SHA-256 in `catalog.json` before anything is installed, and a new package loads at once. In Lua: `require("bone.catalog")` has `index(cb)`, `install(entry, cb)`, `remove(name, cb)` and `open()`. Both sides also have `bone.sha256(text)`, `bone.fs.mkdir(dir)` and `bone.fs.write(path, text)` (whole, atomically).

`/config` (or `/settings`) is the page for all of this: a **General** tab with every TUI option (space or enter toggles a boolean or cycles a choice, enter edits a number or text, `r` resets to the default; an option your `tui.lua` sets anyway is marked), **Providers** (enter uses one; `e` edits one: model, URL, type, reasoning effort, stream usage, API key and key variable, each saved at once, `r` resetting a field to `core.lua`'s and "Undo changes" all of them; `a` adds a provider, `d` deletes one you added), **Plugins** (space switches one off or on, both halves, and it stays that way: `plugins.disabled`), and a tab for each plugin that declares settings. A plugin declares them in its `manifest.json`, which also works for a core-only plugin:

```json
{ "title": "Web search", "settings": [
  { "key": "num_results", "label": "Results per search", "type": "integer",
    "default": 5, "min": 1, "max": 10, "desc": "when the model does not ask for a number" } ] }
```

or from its `tui.lua` with `bone.settings.page{ name = "myplugin", title = "My plugin", fields = { … } }` (the same field shape: `key`, `label`, `type` = `"boolean"`, `"integer"`, `"number"` or `"string"`, `choices`, `default`, `min`, `max`, `desc`). Values are saved as `<name>.<key>`, and the plugin reads them with `bone.settings.get` (falling back to its default). `require("bone.config").open(tab)` opens the page on a tab.

In the TUI: `bone.settings.get(path)`, `bone.settings.all()`, `bone.settings.set(path, value, cb)` and `bone.settings.reset(path, cb)` (paths are dotted, `cb(all, err)` gets every setting after the change), and the `settings/changed` event `{ path, value }`. In the core, `bone.settings.get(path)` and `bone.settings.all()` read the file (changes go through a client).

## Core (`core.lua`)

### `bone.config`

```lua
bone.config.providers.qwen = {
  base_url = "http://localhost:8081/v1",  -- any OpenAI-compatible /chat/completions
  model = "Qwen3.8-27B-exl3-3.8bpw",
  api_key = os.getenv("SOME_KEY"),        -- optional
  reasoning_effort = "medium",            -- optional
  stream_usage = true,                    -- send stream_options.include_usage
}
bone.config.provider = "qwen"      -- which entry to use (optional if there is one)
bone.config.system_prompt = "..."  -- or function(ctx) return "..." end; ctx = { cwd, session_id }
bone.config.data_dir = "~/somewhere"  -- sessions; default is the config dir
bone.config.parallel_tools = true  -- run a reply's read-only tool calls at the same time (default)
bone.config.compact = {            -- summarizing long sessions for the model (these are the defaults)
  keep = 2,                        -- the latest user turns always sent word for word
  auto = true,                     -- compact and retry when the model says the context is too long
  limit = nil,                     -- compact before a call estimated above this many tokens
  provider = nil,                  -- which providers entry writes summaries (default: the current one)
  prompt = nil,                    -- instructions for the summary, replacing the built-in ones
}
```

Compaction keeps a session's transcript whole: a model writes a summary of the older part, kept in the session file, and model calls get the summary in place of those messages (the `context` hook sees them that way; `bone.session.messages` and clients see everything). `/compact` in the TUI (the `session/compact` method) does it by hand; compacting again folds the earlier summary in. `settings.json`'s `compact` (`/config` → Compaction) changes these fields over `core.lua`'s, at once.

The working directory is always appended to the system prompt.

### Tools

```lua
bone.tool.register {
  name = "git_log",
  description = "Show recent commits.",
  parameters = { type = "object", properties = { n = { type = "integer" } } },  -- JSON Schema
  needs_approval = false,   -- metadata for custom approval hooks; does not enforce approval
  parallel = true,          -- only reads: may run alongside other such calls (default false)
  run = function(args, ctx)  -- ctx = { cwd, session_id, call_id }
    local r = bone.system("git log --oneline -n " .. (args.n or 10), { cwd = ctx.cwd })
    if r.code ~= 0 then return nil, r.stderr end   -- an error result
    return r.stdout                                -- or a table (sent as JSON)
  end,
}
```

A Lua tool with the same name as a built-in (`read_file`, `write_file`, `edit_file`, `shell`) replaces it. `error()` inside `run` becomes an error result for the model.

When a reply asks for several tools, consecutive calls of tools that only read run at the same time (up to 8): `read_file`, Lua tools registered with `parallel = true`, and MCP tools their server marks `readOnlyHint`. Any other call runs on its own, after the ones before it and before the ones after it, so writes and shell commands keep their order. Results reach the transcript in the order of the calls. `read_file` lets the model choose its range with `offset`/`limit`; use `full: true` when the whole file is needed. `shell` limits its own output; the `output-cap` example plugin limits every tool's.

- `print(...)` appends to `~/.bone/core.log` (the core may share the terminal with the TUI).

### Hooks

A hook runs at a point in the core with an event table. It returns `nil` (no change), a table of fields to change (later hooks and the core see them), or `{ deny = "why" }` to stop that step. Hooks run by priority (`bone.hook(name, fn, { priority = 10 })`, higher first, default 0), then in registration order: plugins, then `core.lua`. A hook that errors counts as a refusal. Points nobody hooks cost nothing.

| Point | Event | `deny` means |
|---|---|---|
| `turn_start` | `{ session_id, cwd, text, images }`, before the user's message is saved | the turn fails with "why" |
| `system` | `{ session_id, cwd, prompt }`, once per turn: the system prompt (after `bone.config.system_prompt` and the working directory); return `{ prompt = ... }` to change it | the turn fails |
| `context` | `{ session_id, messages }`, before each model call: the messages it will send (system prompt first, then a compacted session's summary in place of what it covers); return `{ messages = ... }` to send different ones. The stored transcript is not changed | the turn fails |
| `request` | `{ session_id, messages, tools }`, before each call to the model, after `context`; return `{ provider = "name", model = "model-id" }` to override this call's provider entry and model (both fields optional). `provider` names a `bone.config.providers` entry; without `model`, that entry's configured model is used. `model` alone keeps the session's selected provider entry. Neither override changes the session's saved selection | the turn fails |
| `request_error` | `{ session_id, error, attempt, model }`, when a model call fails before any output arrived; return `{ retry = ms }` to try again after `ms` (at most 5 retries per call), or `{ retry = ms, provider = "name" }` to send the rest of this call to another `bone.config.providers` entry. Without a retry the turn fails as before | the turn fails |
| `stream` | `{ session_id, turn_id, text, reasoning }`: output as it streams, collected into batches; results are ignored and the stream never waits for it | (ignored) |
| `session_start` | `{ session_id, cwd, new }`, the first time this core uses a session (`new` when it was just created) | (ignored) |
| `queue_add` | `{ session_id, text, mode, images }`, a message about to be sent or queued (`queue/add` or `bone.queue.add`); return `{ text = ..., mode = ... }` to change it | it is refused with "why" |
| `message` | `{ session_id, content, reasoning, tool_calls, usage }`, the model's reply before it is saved | the turn fails |
| `tool_call` | `{ session_id, cwd, id, name, arguments }` | the call is refused; the model sees "why" |
| `tool_result` | `{ session_id, id, name, arguments, output, is_error, images }` (image references; marking the result as an error discards them) | the model sees "why" as an error |
| `turn_end` | `{ session_id, turn_id, outcome }` | (changes are ignored) |

```lua
-- Refuse dangerous commands, add context to every request, trim big outputs:
bone.hook("tool_call", function(ev)
  if ev.name == "shell" and ev.arguments.command:match("rm %-rf /") then return { deny = "not allowed" } end
end)
bone.hook("request", function(ev)
  table.insert(ev.messages, 2, { role = "system", content = "Today is " .. os.date("%A") })
  return { messages = ev.messages }
end)
bone.hook("tool_result", function(ev)
  if #ev.output > 20000 then return { output = ev.output:sub(1, 20000) .. "\n[cut]" } end
end)
```

Any other name is a custom point: plugins can define their own with `bone.hook("my_point", fn)` and run them with `local ev, denied = bone.run_hooks("my_point", ev)`.

### Session transcripts

Hooks and tools can read and change a session's stored transcript (they wait without blocking):

```lua
bone.session.messages(id)                 -- the messages, as providers see them
bone.session.append(id, { role = "user", content = "Remember: be brief." })
bone.session.compact(id, { { role = "user", content = "Summary of earlier work: ..." } })
```

The message queue is open to Lua too: `bone.queue.add(id, text, mode, images)` (`"steer"` or `"next"`; an idle session starts a turn), `bone.queue.list(id)`, `bone.queue.remove(id, queue_id)` and `bone.queue.clear(id)`, with the same effects (and `queue/changed` events) as the protocol's `queue/*` methods.

Sessions of their own, for sub-agents:

```lua
local info = bone.session.create({
  title = "find callers",                       -- default: the first message
  owner = { session_id = ctx.session_id, call_id = ctx.call_id, name = "reviewer" },
})                                              -- cwd defaults to the owner's
local r, err = bone.session.run(info.session_id, "Find every caller of panel_at")
-- r = { session_id, turn_id, text (the final answer), outcome = { status, message? } }
```

`create` emits `session/created`; with an `owner`, clients show the session as the owner's sub-agent (the TUI lists it above the prompt and opens it on a click). `run` starts a turn on an idle session and waits for it without blocking anything else, so several can run at once (a tool registered with `parallel = true`). Cancelling the calling turn cancels this one. The catalog's `subagent` plugin is built on these.

`append` adds a message the model sees from the next call on. `compact` replaces the transcript: the session file keeps every earlier record behind a checkpoint, the session loads from the newest checkpoint, and turn ids keep counting. While a turn runs, the transcript can only change between model calls: in `turn_start`, `system`, `context`, `request` and `request_error` hooks, or after the turn (`turn_end`); elsewhere (a tool, a `tool_call` hook) the call fails, so a message never lands between tool calls and their results. Clients get a `session/updated` event and load the session again. To keep a long session within the model's context without rewriting it, use the core's compaction (`bone.config.compact`) instead; `compact` here replaces the transcript for good and drops a compaction summary.

### Waiting without blocking

Hooks, tools and a `system_prompt` function run as coroutines, so these wait without holding up anything else (other sessions, other hooks):

- `bone.system(cmd, { cwd, stdin, timeout = ms })` runs `bash -c cmd` → `{ code, stdout, stderr }`, or `{ timed_out = true }`.
- `bone.sleep(ms)`.
- `bone.http({ url, method, headers, body, timeout = ms })` → `{ status, headers, body }`. A table `body` is sent as JSON. Connection errors raise; HTTP error statuses don't.

If the turn is cancelled meanwhile, the command or request is stopped and the call returns `nil`. While `core.lua` loads (and inside coroutines of your own) `bone.system` and `bone.sleep` block instead, and `bone.http` is unavailable.

### Asking the user

`bone.ask(question)` pauses a hook, tool or `system_prompt` function until a user answers, and returns the answer (or `nil` if the turn is cancelled first). `question` is any table; clients receive it as an `ask/requested` event and reply with `ask/respond`. Other sessions keep running while one waits.

```lua
bone.hook("tool_call", function(ev)
  if ev.name ~= "write_file" then return end
  local answer = bone.ask({ kind = "confirm", text = "Write " .. ev.arguments.path .. "?" })
  if answer ~= "yes" then return { deny = "the user said no" } end
end)
```

A TUI plugin shows the question however it likes, usually with `bone.ui.popup`, and answers with `bone.request("ask/respond", { ask_id = ev.ask_id, answer = ... })`. See [customizing-popups.md](customizing/customizing-popups.md#confirming-something-from-the-core) for a minimal TUI handler.

### Providers in Lua

Any API can be a model provider. Register one, then pick it with `type` in a providers entry (every field of that entry reaches the provider as `req.options`):

```lua
bone.provider.register("myapi", {
  complete = function(req, emit)
    -- req = { messages, tools = { { name, description, parameters } }, options, session_id }
    local s = bone.http_stream({ url = req.options.base_url .. "/chat", method = "POST", body = { ... } })
    if s.status ~= 200 then error("HTTP " .. s.status .. ": " .. s:text()) end
    local text = ""
    for data in s:events() do            -- each server-sent event's data, as it arrives
      local chunk = bone.json.decode(data).text
      text = text .. chunk
      emit({ text = chunk })             -- or emit({ reasoning = ... })
    end
    return { content = text, tool_calls = {}, usage = { input_tokens = 0, output_tokens = 0 } }
  end,
})
bone.config.providers.mine = { type = "myapi", model = "m", base_url = "https://example.com" }
bone.config.provider = "mine"
```

- `messages` are as in the protocol: `{ role = "system" | "user", content }`, `{ role = "assistant", content, reasoning, tool_calls = { { id, name, arguments } } }` (arguments is a JSON string), `{ role = "tool", call_id, content, is_error }`.
- Return `{ content, reasoning, tool_calls = { { id, name, arguments } }, usage }`; `arguments` may be a string or a table.
- `bone.http_stream(req)` takes the same fields as `bone.http` and returns `{ status, headers }` with `:next()` (the next event's data, `nil` at the end), `:events()` (an iterator), `:text()` (the rest of the body) and `:close()`.
- `complete` runs as a job: it waits without blocking, and a cancelled turn stops it.

For APIs such as Anthropic Messages, implement the translation of messages, tools, streaming events and usage through this provider API; no dedicated Anthropic package is currently published in the catalog.

### MCP servers

bone can use the tools of [MCP](https://modelcontextprotocol.io) servers. None run unless `core.lua` (or a plugin) adds them:

```lua
bone.mcp.add("github", { command = "github-mcp-server", args = { "stdio" }, env = { GITHUB_TOKEN = os.getenv("GITHUB_TOKEN") } })
bone.mcp.add("docs", { url = "https://example.com/mcp", headers = { authorization = "Bearer " .. token } },
  { tools = { allow = { "search" } }, lazy = true, timeout = 60000 })
bone.mcp.load("~/.config/mcp.json")   -- every server in an { "mcpServers": { ... } } file
```

- A server is a program speaking MCP on stdio (`command`, `args`, `env`, `cwd`) or an endpoint speaking Streamable HTTP (`url`, `headers`). Options: `tools = { allow, deny }` (by the server's tool names), `lazy` (start on first use instead of at once) and `timeout` (ms per call, default 120000).
- Its tools reach the model as `<server>_<tool>` (a built-in or Lua tool with the same name wins). They run through the `tool_call` and `tool_result` hooks like any tool; `tool_call` events for them carry `mcp = { server, tool, annotations }`, so a hook can tell (an approval hook can use `readOnlyHint` to distinguish read-only tools).
- Servers run for as long as the core. A reload restarts only the servers whose settings changed (and stops removed ones). A server that exits is restarted after 1, 2, 4, 8 and 16 seconds, then given up on until the next reload; a turn waits up to 10 seconds for servers starting the first time, never for one that is restarting. `/health` shows each server; `mcp/list` gives the same to clients.
- In hooks and tools: `bone.mcp.call(server, tool, args)` → `{ text, is_error }` (by the server's own tool name), and `bone.mcp.list()` → `{ { name, state, error, tools } }`, `state` being `"idle"`, `"starting"`, `"ready"` or `"failed"`. `bone.mcp.remove(name)` takes a server out of the configuration (it stops at the next reload).

Results are text for the model: text parts as they are, a note such as `[image image/png]` for other parts, or the structured content when there is no text.

### Functions clients can call

A plugin whose TUI half needs the core to do something registers a function, and clients call it by name (the `lua/call` method):

```lua
-- core.lua side of a plugin
bone.rpc.register("notes.add", function(args, ctx)   -- ctx = { session_id, cwd }
  local r = bone.system("git rev-parse --short HEAD", { cwd = ctx.cwd })
  return { saved = args.text, at = r.stdout }
end)
```

```lua
-- tui.lua side
bone.rpc.call("notes.add", { text = "remember this" }, function(result, err) end)
```

The function runs as a job, so it may wait (`bone.system`, `bone.model`, `bone.session`), and its return value goes back to the caller as JSON; an error becomes the call's error. Prefix names with the plugin's name. `bone.rpc.unregister(name)`. Skill loading is available through the catalog’s `skill` package; prompt-template loading can be implemented as a customization.

### Calling a model

Hooks, tools and providers can call a model themselves (they wait without blocking, and a cancelled turn stops the call):

```lua
local r, err = bone.model.complete({
  provider = "cheap",                    -- a bone.config.providers key; default: the one turns use
  system = "Answer in three words.",     -- with prompt, or messages = { { role, content }, ... }
  prompt = "Name this session: " .. text,
  options = { model = "small-model" },   -- overrides: model, reasoning_effort, a Lua provider's options
  on_delta = function(d) end,            -- { text = ... } or { reasoning = ... } as it streams
})
-- r = { content, reasoning, tool_calls, usage }, or nil and an error
```

`tools = { { name, description, parameters } }` offers tools: the model may ask for them, and you get `tool_calls` back; nothing runs them. `bone.model.stream(req)` returns a handle instead, read with `h:next()` (the next delta, `nil` at the end), `h:events()`, `h:result()` and `h:close()`. `bone.model.list()` returns `{ name, model, type, current }` for each provider entry. A model call made inside a Lua provider counts as nested; calls nest at most 4 deep, so a provider that ends up calling itself fails instead of looping.

Clients use the same through the protocol (`model/list`, `model/complete`, `model/cancel`); in the TUI that is `bone.model` (see the UI API).

### Health checks

`bone.health(name, fn)` adds a check to `/health` (and the `health/check` method). `fn()` returns a status (`"ok"`, `"warn"`, `"error"`, or `true`/`false`) and a message. Core checks run as jobs, so they may use `bone.system` and `bone.http`:

```lua
bone.health("local model", function()
  local ok, r = pcall(bone.http, { url = "http://localhost:8080/health", timeout = 2000 })
  if not ok then return "error", "not running" end
  return r.status == 200, "status " .. r.status
end)
```

The TUI has its own `bone.health(name, fn)` for TUI-side checks (plain functions; no waiting).

### After loading

`bone.on_ready(fn)` runs `fn()` once after every `core.lua` (the plugins' and yours) has run, before the core serves anything. A plugin's `core.lua` runs before yours, so this is where it can read the final `bone.config`. An error stops the core from starting, like an error in `core.lua`.

### Reloading

The core can load its Lua configuration again without restarting: `/plugins reload` in the TUI, or the `core/reload` method. It starts a fresh Lua state, runs the runtime, the enabled plugins and `core.lua`, and switches only if that worked; if loading fails, the old configuration stays. A turn that is running when this happens finishes with the configuration it started with, and questions it asked can still be answered. The provider, tools, hooks and system prompt all come from the new configuration from the next turn on. `data_dir` cannot change while running (a warning says so).

Lua values do not survive a reload, so keep what must last in `bone.state`. `bone.on_shutdown(fn)` runs `fn()` on the outgoing configuration just before the switch (errors come back as warnings; it gets five seconds).

Core plugins can be switched off and on the same way: `plugin/unload`, `plugin/load` and `plugin/reload` (also `/plugins unload|load|reload name` in the TUI, which handles the plugin's TUI half too). Each is a reload with that plugin left out or put back; the choice lasts until the server restarts. A plugin installed while bone runs is picked up by any reload.

When the TUI runs the core in its own process (not `--connect`), it also reloads the core by itself (with `core/reload`) when a file the core loads changes: `core.lua`, a plugin's `core.lua`, `runtime/core/`, or a module either side can `require` (`lua/`, a plugin's `lua/`, `runtime/lua/`). `bone.o.autoreload = false` turns this off.

### Environment overrides

For one-off runs, these override `core.lua`: `BONE_BASE_URL` + `BONE_MODEL` (use this endpoint), `BONE_MODEL` alone (another model on the configured provider), `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, `BONE_DATA_DIR`.

## TUI (`tui.lua`)

The TUI has no modes: keys go to the prompt, and while a focused window is open (a popup, the session picker, any `bone.ui.select`) its own keys apply first. The built-in contexts are `main`, `popup` and `panel` (while a panel has the keyboard); Lua can define and focus named contexts without changing modeless text entry. Precedence: a focused popup, then a focused panel, then a focused named context, then `main`.

### Reloading

The TUI watches the Lua it loads (`tui.lua`, `lua/`, `colors/`, `runtime/`, the plugins that load, and a trusted project's `.bone/`) and reloads it when a file changes and then stays the same for one more check (a quarter of a second), so a file caught halfway through being saved is not loaded. It reloads the way it starts: a fresh Lua state runs the runtime, the defaults, the plugins, `tui.lua` and the `ready` handlers, so deleting a line really removes what it made and edited modules load anew. Options keep the values you gave them; the session, chats (with items you added), prompt and history are untouched.

The new state loads while the current one keeps running, and it is taken only if loading it raised no Lua error the current one did not also have (the same file and message; only line numbers may differ). Otherwise the current configuration and the built-in options stay as they were, the error is shown (and listed by `/health`), and the next save tries again; requests the failed load already sent to the core are not undone. When it is taken, the old state's `bone.plugin.on_shutdown` hooks run and it is dropped with everything it made (keymaps, commands, event handlers, popups, panels, running `bone.job`s, colors), and replies to its requests are ignored. `bone.plugin.state` tables are saved before the new state loads, so it reads them; what a shutdown hook writes into one during a reload is saved but not seen by the new state, so save plugin state as it changes rather than in `on_shutdown`. Panels do not carry over; `ready` gets `{ reload = true, panels = { … } }`, the ids of the panels that were open, so a plugin can open its panel again.

Files only the core loads, and modules either side can `require`, reload the core too when it runs in the same process (see [Reloading](#reloading); not with `--connect`). `bone.o.autoreload = false` stops watching.

Every key comes from Lua, with one exception: when `ctrl+c` is not mapped where it is pressed (say, a broken or emptied `runtime/tui/defaults.lua`), or is mapped to a Lua function that errors or returns `false`, it runs the built-in `interrupt`, so bone can always be quit.

### Keys

```lua
bone.keymap.set("f2", "/sessions")                            -- a slash command
bone.keymap.set("ctrl+l", "new_session")                      -- a builtin action
bone.keymap.set("alt+s", function() bone.cmd("set noshow_reasoning") end)
bone.keymap.set("ctrl+q", "interrupt", { context = "popup" })  -- while a popup is open
bone.keymap.set("g g", "/health")                            -- a key sequence
bone.keymap.context("review", { fallback = { "main" }, priority = 10 })
bone.keymap.set("r", function() return false end, { context = "review" })
bone.keymap.focus("review")
bone.keymap.clear()                                               -- back to main
local raw = bone.keymap.raw(function(key) return key == "f1" end)
bone.keymap.raw_del(raw)
bone.keymap.del("ctrl+n")
```

A named context defaults to falling back to `main`; `fallback` may be a name or
list and `priority` chooses among fallback branches. An exact mapping that is
also a prefix waits for `bone.o.timeoutlen` milliseconds (default `1000`) before
running the exact mapping. On a mismatch, an exact fallback runs first and the
new key is retried. Lua keymap callbacks consume the key unless they return
`false`; raw interceptors consume only when they return `true`. Context focus is
independent of popups, but a focused popup always has precedence.

Key names: `ctrl+`, `alt+` and `shift+` combined with `enter`, `esc`, `tab`, `backspace`, `delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `space`, `f1`–`f24`, `wheelup`, `wheeldown` (the mouse wheel) or a single character (`"?"`, `"G"`). Whitespace separates members of a sequence. Keys without a mapping type text.

Builtin actions: `submit queue_steer queue_next newline left right up down word_left word_right line_start line_end backspace delete delete_word delete_to_start delete_to_end scroll_up scroll_down page_up page_down scroll_top scroll_bottom complete dismiss interrupt quit quit_if_empty new_session sessions focus_next focus_prev focus_prompt`.

`submit` during a turn queues the message in the core with `bone.o.queue_mode` (`"steer"`, the default, or `"next"`); `queue_steer` and `queue_next` pick the mode regardless. Up on an empty prompt edits the last queued message in place, and the tray's Queue page edits, reorders, steers or drops any of them (`runtime/lua/bone/ui/tray.lua`).

While a panel has the keyboard, `up`/`down` (one row), `scroll_up`/`scroll_down`, `page_up`/`page_down` and `scroll_top`/`scroll_bottom` scroll the panel, and `dismiss` gives the keyboard back to the prompt. `focus_next`/`focus_prev` cycle through the prompt and the focusable panels; `focus_prompt` returns to the prompt.

The defaults are in `runtime/tui/defaults.lua`.

### Options

```lua
bone.o.show_reasoning = false
bone.o.tool_detail = "rows"     -- tool calls: "summary", "rows" or "full" (ctrl+t)
bone.o.tool_preview_lines = 4  -- rows of tool output under each call
bone.o.diff_preview_lines = 8  -- rows of diff under each edit
bone.o.prompt_max_height = 10
bone.o.mouse = false           -- leave the mouse to the terminal (its own selection, no wheel)
bone.o.autoreload = false      -- stop reloading Lua when its files change
```

#### Dynamic options

The compatibility `bone.o.name` read/write syntax remains available. Dynamic
options are registered only in the TUI and are separate from the fixed Rust
options:

```lua
bone.o.apply("show_reasoning!")   -- "name", "noname", "name!", "name=value" or "name?"; returns what "name?" shows
bone.o.list()                     -- every option as "name=value"
bone.o.define("review_limit", 10, {
  type = "integer", desc = "Lines to show in review",
  on_change = function(new, old)
    bone.notify("limit: " .. old .. " -> " .. new)
  end,
})
local value = bone.o.get("review_limit")
local info = bone.o.info("review_limit")
```

Dynamic options support `boolean`, `integer`, `number` and `string`. The
default value is required; its type is inferred unless `type` is supplied, and
an explicit type and default are validated. Unknown names and duplicate
definitions are errors. A string option may list its `choices`
(`{ choices = { "summary", "rows", "full" } }`): any other value is refused,
and `/config` cycles through them.
`on_change` receives `(new, old)` only when the value changes, and is
removed with the option. `bone.o.names()` lists names.
`bone.o.del(name)` returns whether a dynamic option was removed;
`bone.o.delete(name)` is an alias, and deletion permits defining the name
again. `bone.o.info(name)` returns `{ type, value, default, desc }`, plus
`choices` for a dynamic option that has them.

Dynamic options also work with `bone.o.apply`: querying, assigning, enabling,
disabling and toggling booleans use the same validation and callbacks as
assigning `bone.o.name`.
### Commands

```lua
bone.cmd("sessions")                      -- run any slash command (the / is optional)
bone.cmd.create("hello", function(c)      -- lowercase letters, digits, - and _
  bone.notify("hi " .. c.args)
end, { desc = "say hi" })                 -- /hello world
bone.cmd.del("hello")
```


User commands may add aliases and completion/argument specifications:

```lua
bone.cmd.create("greet", function(c)
  -- c.args is the raw argument string; c.argv is its whitespace-split form.
  -- c.arguments and c.parsed contain typed values when args is specified.
  bone.notify(c.arguments.word)
end, {
  desc = "greet someone", aliases = { "hello" },
  args = {
    { name = "count", type = "integer", default = 1 },
    { name = "word", type = "string", required = true },
    { name = "rest", type = "string", variadic = true }, -- must be final
  },
  complete = function(ctx)
    -- ctx = { command, text, args, token, argv }
    return { { value = "world", desc = "the default greeting" }, "there" }
  end,
})
```

`opts.completion` is an alias for `opts.complete`. The command callback gets
`command` as the canonical command name, the raw `args` string, whitespace-split
`argv`, and (when `opts.args` is present) typed `arguments`/`parsed` values.
Arguments can be `string`, `integer`, `number` or `boolean`; defaults are
validated at registration, required arguments must be supplied, and a
variadic argument must be final. Extra or malformed arguments are rejected.
Completion receives the command, full prompt text, raw argument text, current
token and raw `argv`; it may return strings or `{ value, desc }` entries, and
only the current argument token is replaced. Names and aliases are accepted
for lookup, completion and execution. Aliases must not collide with built-in
names or aliases, other user commands/aliases, or the command's own name;
`bone.cmd.del` accepts either the canonical name or an alias.
User commands appear in the `/` suggestions with their `desc`, and in `/help`. Every command is one of these, the defaults included: `/help`, `/health`, `/new` and the rest are defined in `runtime/lua/bone/commands.lua`, so `bone.cmd.create` replaces any of them and `bone.cmd.del` removes it. For Lua from the prompt there is `bone.api.exec_lua(code)` and `bone.api.source(path)`; `bone.api.log(n)` gives the recent messages and full Lua errors.

`bone.cmd.list()` returns `{ { name, desc, aliases, complete } }`, `bone.cmd.find(word)` the command a name or alias means, and `bone.cmd.complete(name, ctx)` runs a command's completion function. Aliases may be any word without spaces or `/` (`/?` is `/help`).

#### The `/` menu

The menu is the Lua module `bone.menu` (`runtime/lua/bone/menu.lua`): it matches what you type against command names and aliases (by prefix, sorted, at most eight rows), or asks a command's completion function for its arguments, and keeps the selection. It is drawn by `bone.ui.suggestions(ctx)` in the `above_prompt` region, so the prompt composer expands to contain the list instead of having a floating box cover it. It takes part in keys through `bone.ui.actions`: `submit` runs `/commands` (the selected match for a partial name; `//text` sends `/text`; `/etc/hosts …` is a message), `complete` (tab) fills in the selection, `dismiss` (esc) hides the menu until the text changes, and `up`/`down` move through it. `require("bone.menu")` gives the module (`all(text)`, `complete()`, `move(by)`); override `~/.bone/runtime/lua/bone/menu.lua` (starting from `bone.builtin("bone.menu")`) for different matching.

#### Actions in Lua

`bone.ui.actions[name] = function() ... end` takes over a builtin action wherever it is used (keymaps, `bone.action`): return `true` when it handled the action, anything else to let the built-in run. Inside its own handler, the action runs the built-in, so a handler can fall back to it.

### Events

```lua
local id = bone.on("turn/finished", function(ev) ... end)
bone.off(id)
```

- Every server notification, by method, with its params: `turn/started`, `turn/steered`, `message/delta`, `message/completed`, `tool/started`, `tool/output`, `tool/finished`, `ask/requested`, `ask/resolved`, `turn/finished`.
- `ready`: after `tui.lua` has run. After a reload its data is `{ reload = true, panels }` (see [Reloading](#reloading-1)); at startup it has none.
- `submit`: `{ text }` before a message is sent. Return `false` to cancel, or a string to send instead.


When `tui.local_events` is available, `bone.on` also accepts local UI events.
Registration aliases normalize to these canonical names:

| Canonical event | Registration aliases | Payload |
|---|---|---|
| `prompt/changed` | `prompt`, `prompt_changed`, `prompt/changed` | `{ text, cursor = { row, col }, selection = { start, end } or nil }` |
| `focus/changed` | `focus`, `focus_changed`, `focus/changed` | `{ context, popup, panel }` (`panel`: the focused panel's id or nil) |
| `resize` | `ui/resize`, `resize` | `{ width, height }` |
| `key` | `key/pressed`, `key` | `{ key, context }` |
| `paste` | `paste`, `text/pasted` | `{ text, context }` |
| `mouse` | `mouse` | `{ action, button, x, y, index, line, region, row, col, panel, panel_line }` |
| `select` | `select` | `{ text }` when a mouse selection ends; return `true` to keep it from the clipboard |
| `panel/opened` | `panel`, `panel/opened`, `popup`, `popup/opened` | a window or a panel (see below) |
| `panel/updated` | `panel/updated`, `popup/updated` | a window or a panel |
| `panel/closed` | `panel/closed`, `popup/closed` | a window or a panel |

| `plugin/loaded`, `plugin/unloaded` | same | `{ name, dir, kind, loaded, error }` |
| `job/started` | `job/started` | `{ id, name, cmd, pid }` |
| `job/finished` | `job/finished` | the job's exit result (see `bone.job`) |
| `process/changed` | `process/changed` | a core-managed shell process snapshot changed |
| `processes/changed` | `processes/changed` | the TUI refreshed its process snapshot |

Prompt events are deduplicated, so unchanged text/cursor/selection state is not emitted.
`mouse` is emitted for every press, drag and release (`action` `"down"`, `"drag"`, `"up"`; `button` `"left"`, `"right"`, `"middle"`; `x`, `y` 0-based screen cells), with `index` and `line` when it is over a chat item, `region`, `row` and `col` (from 1) for the layout leaf under it (a region, `"chat"`, `"prompt"`, `"statusline"`, …), and `panel` and `panel_line` (its content line, from 1; 0 on the title) over a panel. A callback that returns `true` takes it; otherwise the left button selects text and focuses panels as usual. With `bone.chat.redraw(index)` and your own state per item, that is enough for click-to-expand.
`resize` is emitted when terminal dimensions change (and by `Headless:resize`);
`paste` is emitted for bracketed paste with the active context. Popup lifecycle
events are emitted when a `bone.ui.win`/`bone.ui.popup` window or a `bone.ui.panel` opens, updates or closes.
A window's payload is `{ id, kind = "popup", focus, anchor, z, width, height, row, col }`;
a panel's is its `info()` (`kind = "panel"`, see Panels). `panel:set_lines` and scrolling
do not emit `panel/updated`. Local event callbacks are UI-side
callbacks; they do not change core/session ownership.
### Talking to the core

```lua
bone.request("session/list", {}, function(result, err)
  if err then return bone.notify(err, "error") end
  bone.notify(#result .. " sessions")
end)
```

Any protocol method works (see `crates/bone-proto/src/methods.rs`).

### The prompt

`bone.prompt` reads and edits the prompt by position. A position is `{ row, col }` (line and character, both from 0, as in `prompt/changed`) or a character offset from 0 (a line break counts as one); positions past the end are clamped. Edits run `prompt/changed` handlers; nothing is sent until the user submits.

```lua
bone.prompt.get()                      -- the text; bone.prompt.set(text) replaces it
bone.prompt.info()                     -- { text, images, lines, cursor, selection = { start, end, text } or nil }
bone.prompt.lines()
bone.prompt.cursor()                   -- { row, col }
bone.prompt.set_cursor({ row = 0, col = 5 })
bone.prompt.insert(", ")               -- at the cursor, replacing the selection
bone.prompt.get_range(0, 5)            -- text between two positions (either order)
bone.prompt.set_range(0, 5, "hi")      -- replace it; the cursor goes after "hi"
bone.prompt.select(7, 12)              -- select; the cursor moves to the second position (default: where it is)
bone.prompt.selection()                -- { start, end, text } or nil
bone.prompt.clear_selection()
bone.prompt.offset({ row = 1, col = 0 })  -- position -> offset
bone.prompt.position(4)                -- offset -> { row, col }
```

The selection is drawn with the `Selection` group. Typing or pasting replaces it, `backspace`/`delete` and the other delete actions remove it, and cursor movement drops it. `bone.api.prompt_get`/`prompt_set` keep working.

`bone.prompt.images()` returns the draft's image references; `bone.prompt.set_images(refs)` replaces them. `bone.prompt.attach(path)` reads a file asynchronously, `bone.prompt.paste()` reads the local clipboard, and `bone.prompt.remove_image(index)` removes a 1-based attachment. `bone.prompt.empty()` checks text, images and pending reads. Setting text preserves attachments. The `submit` event includes `{ text, images }`; `prompt/changed` includes `images`, and `image/attached` announces `{ image }` after a successful upload. The default `attachments` layout region shows their labels.

In core Lua coroutines, `bone.attachments.upload(base64, name)` yields and returns an `ImageAttachment`; `bone.attachments.read(id)` returns `{ data = base64_png }`. Use references in `{ role = "user", content = "", images = { ref } }`, `bone.queue.add(id, text, mode, { ref })`, session operations or model calls. `read_file` image results include references on their `role = "tool"` messages. Custom providers receive hydrated PNGs as `image.data` on both user and tool messages; translate them to your provider's multimodal format. Stored messages and hook events use references without pixels.

### Chat data

`bone.chat` reads the chats this TUI has open as data. Every call returns fresh copies; changing them changes nothing. `opts.session` picks another open chat by session id (the default is the one on screen; an unknown id gives nothing). The core remains the authority: the TUI's copy is built from protocol events, and `bone.chat.messages` asks the core for the stored transcript.

```lua
bone.chat.items({ kind = "tool", name = "shell", turn = 2 })  -- items as views see them, plus `turn`
bone.chat.items({ kind = "tool", full = true })  -- with output, live and raw_arguments
bone.chat.item(4)                       -- by index, or nil
bone.chat.count({ running = true })     -- tools still running, text still streaming
bone.chat.turns()                       -- one entry per user message
bone.chat.session()                     -- the chat on screen
bone.chat.sessions()                    -- every open chat
bone.chat.messages(function(messages, err) ... end)  -- from the core
bone.chat.add("build", { text = "building…" })   -- an item of your own; returns its id
bone.chat.update(id, { text = "built", ok = true })  -- merge fields, redraw it
bone.chat.remove(id)
bone.chat.at(x, y)                      -- the item at a screen cell: { index, line } or nil
bone.chat.view()                        -- { top, height, rows, follow, first, last }
bone.chat.scroll_to(4, "center")        -- "top" (default), "center" or "bottom"
bone.chat.scroll(-5)                    -- rows (negative is up), or "top" / "bottom"
bone.chat.refresh_in(500)               -- inside a chat view: draw this item again in 500 ms
bone.chat.redraw(4)                     -- draw item 4 again; no index: every item
bone.now()                              -- milliseconds since the epoch
```

- `add(kind, fields, opts)` puts an item of your own after what the chat holds now. `kind` is your name for it (lowercase, not a built-in kind) and `bone.ui.views[kind]` draws it, getting `fields` plus `kind`, `index` and `id`; without a view its `text` shows. It is listed by `items`, can be clicked and scrolled to, and stays where it was put while the answer streams after it. It belongs to the TUI: the core never sees it, and reloading the chat (opening the session again) drops it, so keep anything lasting in `bone.state`. `opts.session` adds to another open chat.
- `items(opts)` filters: `kind` (one, or a list), `around` (an item index: only the unbroken stretch of items of `kind` around it), `name` (tool name), `turn`, `running`, `error` (failed tools and error notices), `from`/`to` (item indexes, inclusive), then `first`/`last` keep the first or last N matches. Items have the fields in the views table plus `turn` (0 for items before the first message), except that tool calls leave out `output`, `live` and `raw_arguments` unless `full = true`: building them is the costly part of a long chat, and views get them on their own item anyway. `item(index, opts)` takes `session` and `full` the same way.
- `turns(opts)` entries: `{ index, text, first, last (item indexes), items, tools, tool_errors, running, outcome, error }`. `outcome` is `"completed"`, `"cancelled"` or `"failed"` (with `error`) for turns that finished while the TUI watched, else nil.
- `session(opts)`: `{ session_id, cwd, created_at, title, new, current, running, starting, queue_paused, turn = { id, elapsed_ms }, items, turns }`, or nil. A chat with no messages yet has `new = true` and no `session_id`.
- `sessions()`: `{ session_id, title, new, current, running }` for each open chat.
- `messages([session_id,] callback)`: `callback(messages, err, result)` with protocol messages (`{ role, content, ... }`, see the Providers section) for that session, default the one on screen (`{}` for a new chat).

### UI API

- `bone.system(cmd, { cwd, stdin, timeout }, function(r, err) ... end)`: run a command in the background; the callback gets `{ code, stdout, stderr }` (or `{ timed_out = true }`), or `nil, err`. The UI never waits.
- `bone.http(req, function(res, err) ... end)`: an HTTP request in the background (same fields as the core's).
- `bone.defer(ms, fn)`: run `fn` later.
- `bone.rpc.call(name, args, function(result, err) end)`: call a function core Lua registered with `bone.rpc.register`, for the session on screen.
- `bone.model.complete(req, on_delta, on_done)` → handle: a model call through the core, outside any session (`req` as the core's `bone.model.complete`, without `on_delta`). `on_delta(d)` gets output as it streams (pass `nil` for none), `on_done(result, err)` the end; `handle:cancel()`. `bone.model.list(function(list, err) end)`.
- `bone.job.start(cmd, opts)` → job: a streaming job (below).
- `bone.notify(msg, level)`: `level` is `"info"` (default) or `"error"`. `print(...)` is `bone.notify`.
- `bone.press("ctrl+c")`: press a key. `bone.action("scroll_top")`: run a builtin action.
- `bone.api.prompt_get()`, `bone.api.prompt_set(text)`, `bone.prompt.history_add(text)` (for up/down recall)
- `bone.api.log(n)` (the last n messages), `bone.api.show(text)` (show without logging), `bone.api.colors_name()`, `bone.api.exec_lua(code)` and `bone.api.source(path)` (run Lua / a Lua file)
- `bone.api.session()` → `{ session_id, cwd, title, running }` or `nil`
- `bone.strwidth(s)`: display width in columns.
- `bone.ui.win{ lines, width, height, anchor, row, col, z, focus, keys, on_key, guard }` → id: a window drawn over the screen. Lua draws every cell; Rust only places it and clears what is behind it.
  - `lines`: a list of lines, or `function(ctx)` returning one (called every frame), with `ctx = { width, height }` the most room it has.
  - `width`, `height`: default to fitting the lines (give a `width` if a line uses `{ fill = ... }`).
  - `anchor`: `"screen"` (default), `"chat"` (the chat area) or `"prompt"` (the space above the prompt; the window sits right on the prompt unless `row` says otherwise).
  - `row`, `col`: in the anchor; default centered, `0` is the top/left edge, negative counts from the bottom/right (`-1` touches it).
  - `z`: stacking order (default 0, newer on top among equals).
  - `focus`: take the keyboard (default `false`). A focused window gets keys first: `keys` maps key names to functions, `on_key(name)` gets the others (return `true` if handled), and the rest go to the `popup` keymap context (`ctrl+c` cancels the turn) or are ignored. `guard`: ms to ignore keys after opening, so typing can't hit it.
- `bone.ui.popup{ ... }`: `bone.ui.win` with `focus = true`.
- `bone.ui.update(id, { ... })`: change any of those fields in place (`false` makes a size or position automatic again). Returns `false` if the window is closed.
- `bone.ui.close(id)`, `bone.ui.is_open(id)`.
- `bone.ui.select(items, { prompt, format, on_choice, loading, empty, footer, width, height })` → handle: a picker in a focused window. Typing filters (matching `format(item)`), `up`/`down` (or `ctrl+p`/`ctrl+n`, the wheel) move, `enter` calls `on_choice(item, index)`, `esc` calls `on_choice(nil)`. `handle:set_items(items)` fills it later (with `loading = true` it shows "loading…" until then); `handle:close()`.
- `bone.ui.sessions()`: the agents sidebar (`ctrl+o`, `/sessions`), implemented in `runtime/lua/bone/ui/sessions.lua` with `bone.ui.panel`. Groups running sessions (animated `ctx.spinner`), recent activity (one hour by default, configurable with `bone.ui.sessions_recent_seconds`) and history; subagents remain hidden in their owner’s tray. Lists titles, activity timestamps, input + output tokens and user-turn counts; supports filtering, keyboard navigation and click-to-open. Switching keeps the panel visible and focuses the prompt. Initial running status comes from `session/active`, then turn events maintain it. Replace it to change how sessions are listed.
- `bone.ui.suggestions(ctx)` → lines: draws the `/` menu (see Commands) right above the prompt, with the closest match nearest the input. `ctx = { items = { { name, desc } }, selected, width, height }`. Set it to `nil` for no list (tab still completes).
- `bone.api.open_session(id)`: show a session.
- `bone.ui.pager(content, { title, width, height })` → handle: scrollable text in a focused window. `content` is a string (wrapped; `#` lines are headings) or a list of lines. `up`/`down`/wheel, `pageup`/`pagedown`, `home`/`end`, `esc` or `q` closes. `handle:set(content)`, `handle:close()`.
- `bone.ui.help(topic)` and `bone.ui.health()`: what `/help {topic}` and `/health` call (in `runtime/tui/defaults.lua`). With no topic, `bone.ui.help()` opens the searchable Commands / Keys / Docs popup. Its browser and topic lookup live in `runtime/lua/bone/help.lua`; override `~/.bone/runtime/lua/bone/help.lua` to customize it. The docs are in `bone._docs`; the TUI's built-in checks come from `bone.api.health()`.
- `bone.ui.box(lines, { title, title_hl, border_hl, width, pad, chars })` → the lines inside a rounded border (`PopupBorder`/`PopupTitle` by default). Only used if you call it.

### Jobs

`bone.system` runs a command and calls back once with all of its output. `bone.job.start` gives a handle to a running process instead: output reaches Lua as it arrives, you can write to it, cancel it, give it a timeout and ask how it is doing. The UI never waits for a job.

```lua
local job = bone.job.start("cargo build --color=never", {
  name = "build",
  lines = true,                               -- whole lines instead of chunks
  on_stdout = function(line, job) ... end,
  on_stderr = function(line, job) progress = line end,
  on_exit = function(r, job)
    bone.notify(r.state == "exited" and ("build: exit " .. r.code) or ("build " .. r.state))
  end,
  timeout = 10 * 60 * 1000,
})
bone.keymap.set("ctrl+x", function() job:cancel() end)
```

- `cmd`: a shell command (run with `bash -c`) or an argv list such as `{ "git", "status", "--short" }`.
- `opts`: `name`; `cwd`; `env` (variables added to the TUI's environment); `stdin` (a string to feed and close, or `true` to keep it open for `job:write(data)` / `job:close_stdin()`); `timeout` (ms); `lines`; `buffer` (keep the output for `on_exit`; the default when neither output callback is given); `on_stdout(data, job)`, `on_stderr(data, job)` and `on_exit(result, job)`.
- Output is text: a character split between two reads is held back until it is complete, and invalid UTF-8 becomes `�`. In line mode `\r\n` and `\n` end lines and a last line without one arrives before `on_exit`.
- `job:cancel()` sends SIGTERM to the job's process group (everything it started), then SIGKILL two seconds later. A timeout does the same. Quitting the TUI kills running jobs.
- `job:status()` (and each entry of `bone.job.list()`): `{ id, name, cmd, pid, state, running, elapsed_ms, stdout_bytes, stderr_bytes }`, plus `code`, `signal` and `error` once it has ended. `state` is `"running"`, `"exited"`, `"cancelled"`, `"timed_out"` or `"failed"` (it could not start; see `error`).
- `on_exit` and the `job/finished` event get the status plus `cancelled`, `timed_out`, `duration_ms` and, when buffering, `stdout`, `stderr` and `truncated` (only the last 4 MiB of each stream are kept).
- `bone.job.list()` has running jobs and the last 32 finished ones; `bone.job.get(id)` returns a handle; `bone.job.cancel_all()`. `job:running()` is a shortcut. The statusline context's `jobs` is the number running.

Core-managed shell processes are separate from plugin jobs. The TUI exposes
`bone.processes.list()`, `bone.processes.refresh()` and
`bone.processes.cancel(id)`; entries carry `tail`, the last line of output.
`bone.processes.screen(id, { width, height, scroll })` gives a job's terminal
at that size as lines of `{ text, group }` (`{ lines, scroll, max }`, scrolled
back `scroll` rows); the first call reads its output, and a running job's
pseudo-terminal is resized to match.

### The tray

`runtime/lua/bone/ui/tray.lua` shows the sub-agents and shell jobs of the
session below the prompt, on two tabs (Agents, Shells), one row each: a
spinner (✓ ✗ ⊘ ⏱ when done), the name and task, what it is doing now, and how
long it has run. `down` on an empty prompt moves into it (the `tray` keymap
context) and `up` from its top row leaves; `tab` switches tabs, `enter` opens
a row (a sub-agent's session in the chat, with `‹ main` in the tray to go
back; a shell job as a live terminal inside the tray), `c` cancels and `esc`
goes back. `ctrl+b` folds it to one line. Finished rows stay until the next
message. `require("bone.ui.tray").rows("agents" | "shells")` gives the rows.

### Colors

Everything is drawn with named highlight groups. `bone.colorscheme("black")` (the default, the original bone palette) and `bone.colorscheme("ansi")` (16 terminal colors) ship with bone; `colors/<name>.lua` in `~/.bone/` or a plugin adds more.

```lua
bone.colorscheme("ansi")
bone.hl.set("ToolPath", { fg = "#7dcfff", bold = true })  -- fg/bg: #rrggbb, a name, or 0-255
bone.hl.set("MyGroup", { link = "ToolError", underline = true })
bone.hl.get("ToolPath")   -- { fg = "#7dcfff", bold = true }
bone.hl.names()
```

`bone.hl.names()` lists the groups.

Groups: `Normal Dim Accent UserPrompt UserMessage Reasoning ToolName ToolArgs ToolPath ToolSummary ToolOutput ToolGutter ToolRunning ToolError DiffAdd DiffDelete ShellProgram ShellPath ShellFlag ShellString ShellVariable ShellComment ShellOperator MdHeading MdBold MdItalic MdCode MdCodeBlock MdQuote MdBullet MdLink Notice ErrorMsg WarningMsg WinSeparator StatusLine StatusLineDim Selection Placeholder PopupBorder PopupTitle PanelTitle PanelTitleFocus`.

### Drawing the screen

Rust keeps the session data, wraps text, caches, scrolls and paints. Everything about how things look is Lua. `runtime/tui/defaults.lua` calls `require("bone.ui").setup(opts)`, the standard UI in `runtime/lua/bone/ui/` (views, statusline, a divider that shows a running turn, the three-row prompt, the empty-session hint, the model reasoning in the chat while it streams, and the layout). Replace any piece from `tui.lua`, or override a module under `~/.bone/runtime/lua/bone/ui/` that starts from `bone.builtin` (see the top of this file). With nothing defined, Rust draws the chat as plain text (`> ` before your messages, one blank line between items, tool calls as name, arguments and output, reasoning hidden) and the bare prompt below it. Message text reaches views without blank lines at its edges.

The standard UI is implemented in `runtime/lua/bone/ui/`; override its callbacks or copy built-in modules with `/runtime` to change the look. The catalog’s `themes` package adds color palettes, not an alternate UI.

The TUI reloads your Lua when it changes (see [Reloading](#reloading-1)). `bone.ui.clear()` removes every view, region, action and UI function, for a config that starts from a blank screen.

#### The prompt

```lua
bone.ui.prompt = {
  prefix = { { "› ", "UserPrompt" } },                 -- a line; following rows are indented to match
  placeholder = { { "Message bone…", "Placeholder" } }, -- shown while the prompt is empty
}
```

Lua draws the prompt's box; Rust keeps its text. Rust wraps the text inside whatever room the box leaves, scrolls it, paints the selection and puts the cursor exactly where the character is, so mouse selection and editing stay right whatever the box looks like. Every field is optional:

| Field | |
|---|---|
| `prefix`, `placeholder` | lines (or functions of `ctx` returning one): before the first row, and shown while empty |
| `continuation` | the line before wrapped and later rows (default: blanks as wide as `prefix`) |
| `border` | `"rounded"`, `"single"`, `"double"`, `"thick"`, `"ascii"`, `true` (rounded), or `{ style, chars = "╭╮╰╯─│", hl = "PromptBorder", sides = "tblr" }`; `sides` picks top, bottom, left, right |
| `background` | a highlight group filling the box |
| `padding` | columns inside the border on each side, or `{ rows, cols }` |
| `min`, `max` | text rows shown at least and at most (default 1 and `bone.o.prompt_max_height`); the box grows with the text in between |
| `top`, `bottom` | a line, or a function of `ctx` returning one, drawn into that edge between the corners (its default group is the border's); it gets its own row when there is no border on that side |

The functions get `ctx = { width, text, lines, empty, cursor = { row, col }, selection = { start, end } or nil, focused, running, elapsed, spinner, session }`; `focused` is false while a popup or panel has the keyboard. `top` and `bottom` run after the text is laid out and also get `rows` (wrapped rows), `height` (rows shown), `text_width`, `scroll` and `cursor.screen = { row, col }`. Positions are from 0, as in `bone.prompt`.

The first bone's input styles, as examples:

```lua
-- "lines": a rule above and below
bone.ui.prompt = { prefix = "> ", border = { style = "single", sides = "tb", hl = "InputBorder" } }
-- "box": rounded, with the state in the bottom edge
bone.ui.prompt = {
  prefix = "> ", border = { style = "rounded", hl = "InputBorder" }, padding = 1,
  bottom = function(ctx)
    if not ctx.running then return nil end
    return { { fill = "─" }, { " " .. ctx.spinner .. " thinking " .. (ctx.elapsed or 0) .. "s ", "Dim" }, "─" }
  end,
}
-- "filled": a background and no border
bone.ui.prompt = { prefix = "> ", background = "InputBackground", padding = { 1, 1 } }
```

The text itself can be styled too. `bone.ui.prompt_highlight(ctx)` (same `ctx`) returns `{ highlights = { { row, from, to, hl }, … }, ghost, ghost_hl }`: each highlight colors chars `from` to `to` (exclusive, from 0) of line `row`, under the selection; `ghost` is text shown after the cursor when it is at the end of its line (in `ghost_hl`, default `Placeholder`), for completions you then accept from a key. Rust keeps the cursor, wrapping and editing. It runs on every frame while the prompt has text, so keep it light.

```lua
bone.ui.prompt_highlight = function(ctx)
  local s, e = ctx.text:find("^/%w+")
  return { highlights = s and { { row = 0, from = s - 1, to = e, hl = "Accent" } } or {} }
end
```

A *line* is a list of items: `{ "text", "Group" }`, a plain string, or `{ fill = "─", hl = "Group" }` (stretches to fill the row). An empty table is a blank line. Lines longer than the width are cut with `…`.

#### Views: how each item looks

The transcript is a list of items. Each kind has a view that returns its lines; return `nil` to hide the item.

```lua
bone.ui.views.reasoning = function(item, ctx)
  return { { { "∴ ", "Dim" }, { item.text, "Reasoning" } } }
end
bone.ui.views.user = function(item, ctx)
  return bone.text.wrap({ { item.text, "UserMessage" } }, ctx.width, { first = "› ", pad = "UserMessage" })
end
```

| Kind | Item fields |
|---|---|
| `user` | `text`, `images` (attachment references) |
| `reasoning`, `assistant` | `text`, `streaming`; `assistant` also `usage = { input, output }` (tokens of that model message, when the core sent them) |
| `tool` | `id`, `name`, `arguments` (decoded), `raw_arguments`, `output` (nil while running), `is_error`, `done`, `live` (output so far while it runs, from `tool/output`; the last 64 KB), `started_at` (ms since the epoch), `duration_ms`, and `usage` when it is the first call of a message without text |
| `notice` | `text`, `error` |
| `queued` | `id`, `text`, `mode` (`"steer"` joins the running turn, `"next"` waits for its own), `position` (1 first); messages waiting in the session's queue, after the transcript. Plain text: `(queued) text` |

Every item also has `kind`, `index` (its place in the chat now, from 1) and `key`: a name that stays the same as items come and go around it (`e12` for a transcript entry, your id for items from `bone.chat.add`, `q…` for queued messages), for keeping your own state per item, such as which ones are expanded. `ctx` is `{ width, region, prev = { kind } }`: `region` is `"chat"` in the transcript, and `prev` lets a view decide spacing (the standard UI adds a blank line except between a message and its tool calls). Assigning a view redraws the chat; a view that errors is reported once and its items fall back to plain text until it is redefined.

Chat views are cached: an item is drawn again when it changes, when the width, options, views or colors change, and when chat data the view read through `bone.chat.items`, `bone.chat.item`, `bone.chat.count` or `bone.chat.turns` changes (so a view may show what comes after its item, or sum up a turn). For time, call `bone.chat.refresh_in(ms)` inside the view and the item is drawn again after that long (a clock, a spinner; `bone.now()` is the time in milliseconds). For anything else, `bone.chat.redraw(index)` draws one item again and `bone.chat.redraw()` all of them; `bone.ui.refresh()` draws every item again (costly in a long chat). Regions, the statusline and popups need none of these: they are drawn on every frame, after any key, event or callback.

For time-based updates in panels, regions, the statusline or popups, call `bone.ui.refresh_in(ms)` from their render function. It schedules a UI frame after `ms` milliseconds without invalidating cached chat views (unlike `bone.ui.refresh()`). The earliest pending request wins; intervening frames keep the deadline, and the request is consumed when due. Call it again during rendering to keep animating. Zero or negative delays request the next frame; redraws remain frame-rate limited. Use `bone.chat.refresh_in(ms)` instead when a cached chat item itself needs to change.

Tool calls have one more layer: `views.tool` draws the frame (status marker, label rows, output), and `bone.ui.tool_views[name](item, ctx)` can supply the content for one tool as `{ title = line, lines = { line, ... } }` (`title` may also be a list of lines; `lines` sit indented under it). Return `nil` for the built-in content (`bone.ui.tool_content`). The standard UI draws tools like the first bone: a label (`shell cmd`, `read_file path (lines 1-20, 20 read)`, `edit_file path (-1 | +2)`), shell output and errors in a `│ ╰` gutter cut to its first and last two rows, edits as a numbered diff, and other tools' first five output lines. `bone.o.tool_detail` (`ctrl+t`) picks how much: `"summary"` (the default) folds each stretch of calls between edits and failures into one line such as `Read 3 files, ran 2 shell commands`, `"rows"` is the above, `"full"` shows every output in full. The summary is drawn by the first call of a stretch: each call reads the item before it (`bone.chat.item`) to tell whether it is first, and only the first reads the stretch, with `bone.chat.items({ around = item.index, from = item.index, kind = { "tool", "reasoning" } })`. It is redrawn when any of them changes, like any view that reads chat data.

#### Regions: content around the chat

```lua
-- The latest reasoning above the prompt while it streams, instead of in the chat.
bone.ui.views.reasoning = function(item, ctx)
  if ctx.region == "chat" then return nil end
  return bone.text.wrap({ { item.text, "Reasoning" } }, ctx.width, { first = "∴ " })
end
bone.ui.regions.above_prompt = {
  size = "auto", max = 3,
  render = function(ctx)
    local r = bone.chat.items({ kind = "reasoning", last = 1 })[1]
    if not r or not r.streaming then return nil end
    return bone.ui.render(r, ctx.width, "above_prompt")
  end,
}
bone.ui.regions.right = { size = 30, render = function(ctx) return { "notes" } end }
```

Regions are placed by `bone.ui.layout`, which lists the screen's rows from top to bottom. The default puts `left` and `right` as columns beside the chat (empty until you define those regions):

```lua
bone.ui.layout = { "top", { cols = { "left", "chat", "right" }, sep = "│" }, "divider", "above_prompt", "prompt", "statusline" }  -- the default
bone.ui.layout = { "statusline", "chat", "mybar", "prompt" }  -- statusline on top, a custom band
bone.ui.regions.mybar = { size = 1, render = function(ctx) return { "hello" } end }
```

The layout is a tree. An entry is a name, `{ "name", size = … }`, or a split: `{ rows = { … } }` stacks its entries, `{ cols = { … }, sep = "│" }` puts them side by side (with `sep` drawn between), and splits nest:

```lua
bone.ui.layout = {
  "statusline",
  { cols = {
      { "files", size = "25%" },              -- a region as a column
      { rows = { "chat", "above_prompt" } },  -- the chat with a band under it
      { "notes", size = 30 },
    }, sep = "│" },
  "message",                                  -- notifications here, not at the bottom
  "prompt",
}
```

`size` is cells (rows in a stack, columns side by side), `"30%"`, `"auto"` (its natural size) or `"fill"` (a share of what is left). By default the chat and splits fill; the prompt, statusline, divider and message line take their natural size; and regions take theirs from their own `size`. Fixed and percent sizes are given out first, then natural sizes (regions leave a filling sibling at least 3 rows), then filling entries share the rest. A region given a fixed, percent or fill size is drawn at exactly that size.

The terminal's title is `bone.ui.title(ctx)` (the statusline's `ctx`), set when it changes; leave it undefined to keep the terminal's own. The spinner every `ctx.spinner` shows is `bone.ui.spinner = { frames = { "◐", "◓", "◑", "◒" }, interval = 120 }` (milliseconds per frame); unset, it is the default braille dots, and the screen redraws at that pace while a turn runs.

`chat`, `prompt`, `divider`, `statusline` and `message` are built in; leave one out and it is not shown (without `message`, notifications take rows at the bottom). Row regions are sized in rows. `size` is a number or `"auto"` (fit the content, up to `max`, default 10); a bare function means `size = "auto"`. `render(ctx)` gets `{ region, width, height, spinner, popup, session }` and returns lines, or `nil` to hide the region. Regions are redrawn every frame, so keep them light.

- `bone.chat.items({ kind = "tool", last = 3 })` returns items of the session on screen, as views receive them (more filters under Chat data).
- `bone.ui.render(item, width, region)` renders an item with the current views.

#### Panels in the layout tree

Use `{ panel = "id", size = … }` or `"panel:id"` wherever a region leaf
can appear; open the panel normally. See the [panel guide](customizing/customizing-panels.md)
for an example.

- Sizing: defaults to `"fill"`; cells, `"N%"`, `"auto"` and `"fill"` override
  dock sizing. Auto measures along the parent split (including titles for rows),
  capped by panel `max`. Fixed/percent and natural sizes precede fill; allocations
  clamp in tree order. Dock `min` and chat minimums do not apply to tree leaves.
- Placement: a tree reference suppresses docking and `full_height`; removing it
  restores docking. Use each ID once (only its first nonempty leaf is painted).
  Tree panels use split `sep`. Hidden/missing panels and subtrees containing only
  those panels reserve neither space nor separators.
- Rendering: content callbacks may run for measurement, then with final `width`
  and `height` (excluding panel titles). Keep measurement free of side effects;
  sizing is one pass, not a fixed-point iteration. Prompt rendering and mouse
  hits use final rectangles. Docked cells are excluded from the `chat` leaf;
  tree mouse `region` is `"panel:id"`, with usual `panel`/`panel_line` fields.
- Mutations: layout changes during rendering apply next frame. Hiding/closing a
  panel clears its cells, geometry and hit targets immediately; reserved space
  is reclaimed next frame. Floating windows/popups never consume layout space.

#### Panels: docked areas you own

Panels are windows you open and close at runtime, with focus, keys and scrolling of their own. They can dock beside the chat or occupy a leaf in `bone.ui.layout`. Regions remain useful for stateless bands without panel focus or scrolling.

A panel is a persistent area beside, above or below the chat. It takes room from the chat (unlike `bone.ui.win`, which floats over it), keeps its scroll position while you do other things, can be hidden and shown again, and can take the keyboard. Lua supplies all of its content; Rust sizes and places it, scrolls it, and routes keys and the mouse wheel to it.

```lua
local todo = { "write tests", "update docs" }
local p = bone.ui.panel.open({
  id = "todo",                  -- default "panel<N>"; letters, digits, _ - .
  dock = "right",               -- "right" (default), "left", "top", "bottom"
  size = 32,                    -- see below
  title = "Todo",               -- a row above the content that does not scroll
  render = function(ctx)        -- every frame; or lines = { ... }
    local out = {}
    for i, t in ipairs(todo) do
      out[i] = { { (ctx.focused and "• " or "  ") .. t, "Normal" } }
    end
    return out
  end,
  keys = {
    d = function(panel) table.remove(todo, 1) end,  -- done with the first
  },
  on_key = function(key, panel) return false end,  -- true if handled
  on_close = function(panel) bone.notify("bye") end,
})
bone.keymap.set("f3", function() p:focus() end)
```

- `size`: columns for `left`/`right`, rows for `top`/`bottom`. A number of cells, a fraction of the room (`0.3`), or `"auto"` to fit the content up to `max` (default 40 columns or 10 rows). The default is 30 columns beside the chat and `"auto"` above or below it. The chat always keeps 20 columns and 3 rows; a panel that would get less than `min` (default 1) is not drawn that frame (`info().visible` is false).
- Placement: `left` and `right` panels normally span the chat viewport (after any `left`/`right` regions), then `top` and `bottom` panels split the chat column. Set `full_height = true` on a side panel to span the entire screen and keep the prompt, tray and statusline beside it; the conversation sidebar uses this. Among panels on the same side, lower `order` (default 0) is placed nearer the edge, then older first. Side panels get a `WinSeparator` bar next to the chat; `separator = false` leaves that column blank.
- Content: `render(ctx)` returns every line (or nil for none), with `ctx = { id, dock, width, height, focused, top, title }`; for `"auto"` it is first called with measurement room, then with the final body size. `lines` is a fixed list instead (a table you keep changing is fine, it is read every frame). Rust shows the rows from `top`; `follow = true` keeps the end in view as content grows, until it is scrolled away from it. A render error is reported once and leaves the panel empty until `update`/`set_lines` gives it new content.
- The keyboard: `focus = true` (or `p:focus()`, `bone.ui.panel.focus(id)`, a click on it, `focus_next`) gives a `focusable` panel the keyboard. Its `keys` run first, then `on_key(key, panel)`, then the `panel` keymap context (or the panel's own `context`, a named context; give it `fallback = "panel"` to keep the scroll keys). The defaults map the arrows, page keys, `home`/`end` and the wheel to scrolling, `esc` back to the prompt, `tab`/`shift+tab` to the next/previous panel and `ctrl+c` to `interrupt`. Text that is not mapped is ignored. The wheel over any panel scrolls that panel.
- Lifecycle: `panel/opened`, `panel/updated` and `panel/closed` events (`kind = "panel"`), and `on_close(panel)` when it closes. Hiding or closing the focused panel gives the keyboard back to the prompt. Panels stay open across sessions until closed.

`bone.ui.panel.open(spec)` (or `bone.ui.panel(spec)`) returns a handle:

| Call | Does |
|---|---|
| `p:update(spec)` | change any field above (`false` resets `size`, `max`, `title`, `context`); `focus = true/false`. Returns `false` if it is closed |
| `p:set_lines(lines)` | replace the content (a list, or a render function) |
| `p:scroll(n)`, `p:scroll("top")`, `p:scroll("bottom")` | scroll by rows or to an end |
| `p:hide()`, `p:show()`, `p:toggle()` | take it off the screen and back, keeping its state |
| `p:focus()`, `p:close()`, `p:is_open()` | |
| `p:info()` | `{ id, kind, dock, size, order, title, hidden, focusable, focus, follow, top, rows, visible, width, height, row, col }` (the last four from the last frame, while visible) |

`bone.ui.panel.get(id)` returns a handle or nil, `bone.ui.panel.list()` every panel's info in placement order, `bone.ui.panel.focused()` the focused panel's id, and `bone.ui.panel.update(id, spec)`, `close(id)` and `focus(id)` (nil for the prompt) work by id.

#### Statusline and divider

```lua
bone.ui.statusline = function(ctx)
  return { { " " .. ctx.title, "StatusLine" }, "%=", { ctx.spinner, "Accent" }, " " }
end
bone.ui.divider = function(ctx)   -- the line between the chat and the prompt
  return { { fill = "─", hl = "WinSeparator" } }
end
```

Each returns one line (`"%="` is a blank stretch); the row exists only while the function is defined. The statusline context has `title`, `popup` (the active keymap context unless it is `main`: `"popup"` while a focused window is open), `panel` (the focused panel's id, else nil), `jobs` (running jobs), `spinner`, `width` and `session` (`{ title, cwd, running, elapsed, usage = { input, output } }` or nil); the divider context has `spinner`, `width` and `session`. If one errors, its row is blank until it is redefined.

#### Helpers

- `bone.markdown.parse(text)` → blocks: `{ kind = "heading", level, spans }`, `"paragraph"` / `"item"` (`indent`, and for items `marker`, `ordered`), `"quote"`, `"code"` (`lang`, `lines`), `"rule"`, `"blank"`. Spans are `{ text, bold?, italic?, code?, link? }`.
- `bone.ui.markdown(text, width, indent)` → the standard UI's rendering of that, as lines.
- `bone.text.wrap(spans, width, { first, rest, pad })` → lines. `first`/`rest` prefix the first and following rows (`rest` defaults to blanks as wide as `first`); `pad = "Group"` fills each row to the width (background bands).
- `bone.text.clip(spans, width)`, `bone.text.truncate(s, n)`, `bone.text.width(s)`.
- `bone.text.shell(cmd)` → the command as highlighted spans.
- `bone.ui.preview(text, max, group)` → at most `max` rows: first line, `⋮ +N lines`, last lines.

Errors in Lua never crash bone: they show as a one-line message, with the full traceback in `bone.api.log`.
