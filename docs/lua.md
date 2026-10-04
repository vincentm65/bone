# Lua in bone

bone runs two separate LuaJIT states, one per side:

| File | Runs in | Controls |
|---|---|---|
| `~/.bone/core.lua` | the core (server) | providers, system prompt, tools, hooks |
| `~/.bone/tui.lua` | the TUI | options, keys, commands, colors, events |

`$BONE_CONFIG_DIR` replaces `~/.bone`. The two states never share Lua values; they talk only through the bone protocol. Lua has full trust (no sandbox). Tool calls run without asking; asking first is a plugin (`examples/plugins/approve`, see below).

Load order on each side: `runtime/<side>/api.lua`, then `runtime/<side>/defaults.lua`, then your file. The runtime is built into the binary, and any runtime file can be replaced by putting a file at the same path under `~/.bone/runtime/` (for example `~/.bone/runtime/tui/defaults.lua` drops every default keymap).

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
| Plugin state | Folder plugins load in name order between runtime defaults and the user's config; `bone.plugins` reports names and `.`/`_` folders are disabled. There is no persistent state store, reload hook or automatic resource cleanup yet. | In the TUI, plugins own what they create; `bone.plugin.on_shutdown`, load/unload/reload (`/plugin`), auto-saved `bone.plugin.state` and trusted project config (`/project`) are available through `plugins.lifecycle` and `tui.project`. Both sides have `bone.state` and `bone.plugin.current()` (`plugins.state`). |

The existing detailed sections describe both the compatibility calls and the
new additive APIs. A plugin may use a compatibility API today and opt into
each later capability independently.

## Plugins

A plugin is a folder in `~/.bone/plugins/`:

```text
~/.bone/plugins/git/
  core.lua     runs in the core (tools, hooks)
  tui.lua      runs in the TUI (keys, commands, tool views)
  lua/         modules for require()
  colors/      colorschemes
```

The repo's examples show most of the API at work: `style` (a complete look), `approve` (asking before tools run), `git` and `anthropic` (tools and a provider), `switch` (pick the provider from the TUI), `tasks` (a persistent task panel), `review` (the files a session changed, and review prompts), `stats` (usage and tool numbers from `store/query`), `testrun` (tests streamed into a panel), `compact` (summarize long sessions, by hand or when the context is full), `output-cap` (limit any tool result's size), `retry` (back off and fall back on passing errors), `mcp` (MCP servers from JSON files, and a status panel), `skills` and `templates` (load folders of them, with TUI commands) and `ask-model` (a side question to a model). None is installed by default.

Every part is optional. Plugins load in name order, after the runtime defaults and before your own `core.lua` / `tui.lua`, so your config can change anything a plugin set up. Rename a folder to start with `_` or `.` to disable it. `bone.plugins` lists the loaded names. Installing is just copying or `git clone`-ing into `~/.bone/plugins/`; see `examples/plugins/` in the repo (`git`, `approve`, `style`).

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
- `bone.plugin.load(name)` (a folder in `plugins/`, even one added after startup or disabled with `_`), `bone.plugin.unload(name)`, `bone.plugin.reload(name)`; also `/plugin load|unload|reload name`. A plugin cannot unload itself from its own code.
- `bone.plugin.state(name)` returns a table that is loaded once and saved when its plugin unloads and when bone quits; `bone.plugin.save_state(name)` saves it now. `name` defaults to the current plugin.
- Events: `plugin/loaded` and `plugin/unloaded` with the plugin's info.

### Project config

A project can carry TUI config in `.bone/tui.lua` (and modules in `.bone/lua/`). bone looks for it in the working directory and its parents, but Lua has full trust, so it only runs after you trust that directory: `/project trust` runs it now and on later starts there, `/project untrust` unloads it and forgets the trust, and `/project` shows the state. Until then bone says that there is one. It loads after your own `tui.lua`, as a plugin named `project`, so it can be unloaded like any other. `bone.project.info()` returns `{ root, file, trusted, loaded }` or nil.

## Shared

- `bone.side`: `"core"` or `"tui"`. `bone.version`, `bone.config_dir`.
- `bone.json.encode(value)`, `bone.json.decode(string)`
- `bone.inspect(value)`: readable dump of any value.
- `bone.util`: `split(s, sep)`, `trim(s)`, `startswith(s, prefix)`, `extend(t1, t2, ...)`, `front_matter(text)` → `meta, body` for Markdown files that start with `---` / `key: value` lines / `---`.
- `bone.fs.list(dir)` → `{ { name, type = "file" | "dir" } }`, sorted (`~/` works), or `nil` and an error.

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
```

The working directory is always appended to the system prompt.

### Tools

```lua
bone.tool.register {
  name = "git_log",
  description = "Show recent commits.",
  parameters = { type = "object", properties = { n = { type = "integer" } } },  -- JSON Schema
  needs_approval = false,   -- read by the approve plugin (default: ask)
  parallel = true,          -- only reads: may run alongside other such calls (default false)
  run = function(args, ctx)  -- ctx = { cwd, session_id }
    local r = bone.system("git log --oneline -n " .. (args.n or 10), { cwd = ctx.cwd })
    if r.code ~= 0 then return nil, r.stderr end   -- an error result
    return r.stdout                                -- or a table (sent as JSON)
  end,
}
```

A Lua tool with the same name as a built-in (`read_file`, `write_file`, `edit_file`, `shell`) replaces it. `error()` inside `run` becomes an error result for the model.

When a reply asks for several tools, consecutive calls of tools that only read run at the same time (up to 8): `read_file`, Lua tools registered with `parallel = true`, and MCP tools their server marks `readOnlyHint`. Any other call runs on its own, after the ones before it and before the ones after it, so writes and shell commands keep their order. Results reach the transcript in the order of the calls. Results are not cut (only `shell` limits its own output); the `output-cap` example plugin limits every tool's.

- `print(...)` appends to `~/.bone/core.log` (the core may share the terminal with the TUI).

### Hooks

A hook runs at a point in the core with an event table. It returns `nil` (no change), a table of fields to change (later hooks and the core see them), or `{ deny = "why" }` to stop that step. Hooks run by priority (`bone.hook(name, fn, { priority = 10 })`, higher first, default 0), then in registration order: plugins, then `core.lua`. A hook that errors counts as a refusal. Points nobody hooks cost nothing.

| Point | Event | `deny` means |
|---|---|---|
| `turn_start` | `{ session_id, cwd, text }`, before the user's message is saved | the turn fails with "why" |
| `system` | `{ session_id, cwd, prompt }`, once per turn: the system prompt (after `bone.config.system_prompt` and the working directory); return `{ prompt = ... }` to change it | the turn fails |
| `context` | `{ session_id, messages }`, before each model call: the messages it will send (system prompt first); return `{ messages = ... }` to send different ones. The stored transcript is not changed | the turn fails |
| `request` | `{ session_id, messages, tools }`, before each call to the model, after `context` | the turn fails |
| `request_error` | `{ session_id, error, attempt, model }`, when a model call fails before any output arrived; return `{ retry = ms }` to try again after `ms` (at most 5 retries per call), or `{ retry = ms, provider = "name" }` to send the rest of this call to another `bone.config.providers` entry. Without a retry the turn fails as before | the turn fails |
| `stream` | `{ session_id, turn_id, text, reasoning }`: output as it streams, collected into batches; results are ignored and the stream never waits for it | (ignored) |
| `session_start` | `{ session_id, cwd, new }`, the first time this core uses a session (`new` when it was just created) | (ignored) |
| `queue_add` | `{ session_id, text, mode }`, a message about to be sent or queued (`queue/add`); return `{ text = ..., mode = ... }` to change it | it is refused with "why" |
| `message` | `{ session_id, content, reasoning, tool_calls, usage }`, the model's reply before it is saved | the turn fails |
| `tool_call` | `{ session_id, cwd, id, name, arguments }` | the call is refused; the model sees "why" |
| `tool_result` | `{ session_id, id, name, arguments, output, is_error }` | the model sees "why" as an error |
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

The message queue is open to Lua too: `bone.queue.add(id, text, mode)` (`"steer"` or `"next"`; an idle session starts a turn), `bone.queue.list(id)`, `bone.queue.remove(id, queue_id)` and `bone.queue.clear(id)`, with the same effects (and `queue/changed` events) as the protocol's `queue/*` methods.

`append` adds a message the model sees from the next call on. `compact` replaces the transcript: the session file keeps every earlier record behind a checkpoint, the session loads from the newest checkpoint, and turn ids keep counting. While a turn runs, the transcript can only change between model calls: in `turn_start`, `system`, `context`, `request` and `request_error` hooks, or after the turn (`turn_end`); elsewhere (a tool, a `tool_call` hook) the call fails, so a message never lands between tool calls and their results. Clients get a `session/updated` event and load the session again. Compacting itself (deciding what to keep, writing a summary) is up to a plugin.

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

A TUI plugin shows the question however it likes, usually with `bone.ui.popup`, and answers with `bone.request("ask/respond", { ask_id = ev.ask_id, answer = ... })`. The built-in `approve` plugin is a complete example (`runtime/plugins/approve/`).

### The approve plugin

Not installed by default: bone runs every tool call without asking. Install it with `cp -r examples/plugins/approve ~/.bone/plugins/`. Then, before `write_file`, `edit_file`, `shell` and Lua tools (unless registered with `needs_approval = false`) it asks `{ kind = "approval", title, tool, arguments }`. The TUI shows `y` allow, `a` always (this tool, this session), `n`/`esc` deny.

```lua
bone.config.approve.enabled = false              -- never ask
bone.config.approve.tools.shell = false          -- don't ask for shell
bone.config.approve.allow = function(ev)         -- skip asking when this returns true
  return ev.name == "shell" and ev.arguments.command:match("^git status")
end
```

`BONE_APPROVAL=auto` turns it off for one run. It is about 60 lines of core Lua plus a popup; copy and change it to build your own rules.

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

`examples/plugins/anthropic` is a complete provider for the Anthropic Messages API (streaming, thinking, tools) in about 140 lines.

### MCP servers

bone can use the tools of [MCP](https://modelcontextprotocol.io) servers. None run unless `core.lua` (or a plugin) adds them:

```lua
bone.mcp.add("github", { command = "github-mcp-server", args = { "stdio" }, env = { GITHUB_TOKEN = os.getenv("GITHUB_TOKEN") } })
bone.mcp.add("docs", { url = "https://example.com/mcp", headers = { authorization = "Bearer " .. token } },
  { tools = { allow = { "search" } }, lazy = true, timeout = 60000 })
bone.mcp.load("~/.config/mcp.json")   -- every server in an { "mcpServers": { ... } } file
```

- A server is a program speaking MCP on stdio (`command`, `args`, `env`, `cwd`) or an endpoint speaking Streamable HTTP (`url`, `headers`). Options: `tools = { allow, deny }` (by the server's tool names), `lazy` (start on first use instead of at once) and `timeout` (ms per call, default 120000).
- Its tools reach the model as `<server>_<tool>` (a built-in or Lua tool with the same name wins). They run through the `tool_call` and `tool_result` hooks like any tool; `tool_call` events for them carry `mcp = { server, tool, annotations }`, so a hook can tell (the approve plugin asks for MCP tools unless the server marks them `readOnlyHint`).
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

The function runs as a job, so it may wait (`bone.system`, `bone.model`, `bone.session`), and its return value goes back to the caller as JSON; an error becomes the call's error. Prefix names with the plugin's name. `bone.rpc.unregister(name)`. The example `skills`, `templates` and `compact` plugins work this way; skills and prompt templates themselves are plugins, not part of the core.

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

`bone.on_ready(fn)` runs `fn()` once after every `core.lua` (the plugins' and yours) has run, before the core serves anything. A plugin's `core.lua` runs before yours, so this is where it can read the final `bone.config` (the `switch` example publishes the provider list from here). An error stops the core from starting, like an error in `core.lua`.

### Reloading

The core can load its Lua configuration again without restarting: `/plugin reload` in the TUI, or the `core/reload` method. It starts a fresh Lua state, runs the runtime, the enabled plugins and `core.lua`, and switches only if that worked; if loading fails, the old configuration stays. A turn that is running when this happens finishes with the configuration it started with, and questions it asked can still be answered. The provider, tools, hooks and system prompt all come from the new configuration from the next turn on. `data_dir` cannot change while running (a warning says so).

Lua values do not survive a reload, so keep what must last in `bone.state`. `bone.on_shutdown(fn)` runs `fn()` on the outgoing configuration just before the switch (errors come back as warnings; it gets five seconds).

Core plugins can be switched off and on the same way: `plugin/unload`, `plugin/load` and `plugin/reload` (also `/plugin unload|load|reload name` in the TUI, which handles the plugin's TUI half too). Each is a reload with that plugin left out or put back; the choice lasts until the server restarts. A plugin installed while bone runs is picked up by any reload.

### Environment overrides

For one-off runs, these override `core.lua`: `BONE_BASE_URL` + `BONE_MODEL` (use this endpoint), `BONE_MODEL` alone (another model on the configured provider), `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, `BONE_DATA_DIR`, and `BONE_APPROVAL=auto` (the approve plugin, if installed, never asks).

## TUI (`tui.lua`)

The TUI has no modes: keys go to the prompt, and while a focused window is open (a popup, the session picker, any `bone.ui.select`) its own keys apply first. The built-in contexts are `main`, `popup` and `panel` (while a panel has the keyboard); Lua can define and focus named contexts without changing modeless text entry. Precedence: a focused popup, then a focused panel, then a focused named context, then `main`.

### Keys

```lua
bone.keymap.set("f2", "/sessions")                            -- a slash command
bone.keymap.set("ctrl+l", "new_session")                      -- a builtin action
bone.keymap.set("alt+s", function() bone.cmd("set noshow_reasoning") end)
bone.keymap.set("ctrl+q", "interrupt", { context = "popup" })  -- while a popup is open
bone.keymap.set("g g", "/messages")                            -- a key sequence
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

`submit` during a turn queues the message in the core with `bone.o.queue_mode` (`"steer"`, the default, or `"next"`); `queue_steer` and `queue_next` pick the mode regardless. Up on an empty prompt takes the last queued message back to edit (`runtime/tui/defaults.lua`).

While a panel has the keyboard, `up`/`down` (one row), `scroll_up`/`scroll_down`, `page_up`/`page_down` and `scroll_top`/`scroll_bottom` scroll the panel, and `dismiss` gives the keyboard back to the prompt. `focus_next`/`focus_prev` cycle through the prompt and the focusable panels; `focus_prompt` returns to the prompt.

The defaults are in `runtime/tui/defaults.lua`.

### Options

```lua
bone.o.show_reasoning = false
bone.o.tool_preview_lines = 4  -- rows of tool output under each call
bone.o.diff_preview_lines = 8  -- rows of diff under each edit
bone.o.prompt_max_height = 10
bone.o.mouse = false           -- leave the mouse to the terminal (its own selection, no wheel)
```

#### Dynamic options

The compatibility `bone.o.name` read/write syntax remains available. Dynamic
options are registered only in the TUI and are separate from the fixed Rust
options:

```lua
bone.o.apply("show_reasoning!")   -- a /set argument; returns what "name?" shows
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
definitions are errors.
`on_change` receives `(new, old)` only when the value changes, and is
removed with the option. `bone.o.names()` lists names.
`bone.o.del(name)` returns whether a dynamic option was removed;
`bone.o.delete(name)` is an alias, and deletion permits defining the name
again. `bone.o.info(name)` returns `{ type, value }` for a fixed
option, and adds `{ default, desc }` for a dynamic option.

Dynamic options also participate in `/set`: querying, assigning, enabling,
disabling, toggling booleans and listing options use the same validation and
callbacks as the Lua API.
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
User commands appear in the `/` suggestions with their `desc`, and in `/help`. Every command is one of these, the defaults included: `/help`, `/set`, `/lua` and the rest are defined in `runtime/lua/bone/commands.lua`, so `bone.cmd.create` replaces any of them and `bone.cmd.del` removes it. Commands related to Lua: `/lua code`, `/lua =expr` (show a value), `/source file`, `/messages` (recent messages, full Lua errors).

`bone.cmd.list()` returns `{ { name, desc, aliases, complete } }`, `bone.cmd.find(word)` the command a name or alias means, and `bone.cmd.complete(name, ctx)` runs a command's completion function. Aliases may be any word without spaces or `/` (`/?` is `/help`).

#### The `/` menu

The menu is the Lua module `bone.menu` (`runtime/lua/bone/menu.lua`): it matches what you type against command names and aliases (by prefix, sorted, at most `max = 8`), or asks a command's completion function for its arguments, and keeps the selection. It is drawn by `bone.ui.suggestions(ctx)` in a window resting on the prompt. It takes part in keys through `bone.ui.actions`: `submit` runs `/commands` (the selected match for a partial name; `//text` sends `/text`; `/etc/hosts …` is a message), `complete` (tab) fills in the selection, `dismiss` (esc) hides the menu until the text changes, and `up`/`down` move through it. `require("bone.menu")` gives the module (`matches(text)`, `complete()`, `move(by)`, `max`); replace the file under `~/.bone/runtime/lua/bone/menu.lua` for different matching.

#### Actions in Lua

`bone.ui.actions[name] = function() ... end` takes over a builtin action wherever it is used (keymaps, `bone.action`): return `true` when it handled the action, anything else to let the built-in run. Inside its own handler, the action runs the built-in, so a handler can fall back to it.

### Events

```lua
local id = bone.on("turn/finished", function(ev) ... end)
bone.off(id)
```

- Every server notification, by method, with its params: `turn/started`, `turn/steered`, `message/delta`, `message/completed`, `tool/started`, `tool/finished`, `ask/requested`, `ask/resolved`, `turn/finished`.
- `ready`: after `tui.lua` has run.
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
| `panel/opened` | `panel`, `panel/opened`, `popup`, `popup/opened` | a window or a panel (see below) |
| `panel/updated` | `panel/updated`, `popup/updated` | a window or a panel |
| `panel/closed` | `panel/closed`, `popup/closed` | a window or a panel |

| `plugin/loaded`, `plugin/unloaded` | same | `{ name, dir, kind, loaded, error }` |
| `job/started` | `job/started` | `{ id, name, cmd, pid }` |
| `job/finished` | `job/finished` | the job's exit result (see `bone.job`) |

Prompt events are deduplicated, so unchanged text/cursor/selection state is not emitted.
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
bone.prompt.info()                     -- { text, lines, cursor, selection = { start, end, text } or nil }
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

### Chat data

`bone.chat` reads the chats this TUI has open as data. Every call returns fresh copies; changing them changes nothing. `opts.session` picks another open chat by session id (the default is the one on screen; an unknown id gives nothing). The core remains the authority: the TUI's copy is built from protocol events, and `bone.chat.messages` asks the core for the stored transcript.

```lua
bone.chat.items({ kind = "tool", name = "shell", turn = 2 })  -- items as views see them, plus `turn`
bone.chat.item(4)                       -- by index, or nil
bone.chat.count({ running = true })     -- tools still running, text still streaming
bone.chat.turns()                       -- one entry per user message
bone.chat.session()                     -- the chat on screen
bone.chat.sessions()                    -- every open chat
bone.chat.messages(function(messages, err) ... end)  -- from the core
```

- `items(opts)` filters: `kind`, `name` (tool name), `turn`, `running`, `error` (failed tools and error notices), `from`/`to` (item indexes, inclusive), then `first`/`last` keep the first or last N matches. Items have the fields in the views table plus `turn` (0 for items before the first message).
- `turns(opts)` entries: `{ index, text, first, last (item indexes), items, tools, tool_errors, running, outcome, error }`. `outcome` is `"completed"`, `"cancelled"` or `"failed"` (with `error`) for turns that finished while the TUI watched, else nil.
- `session(opts)`: `{ session_id, cwd, created_at, title, new, current, running, starting, turn = { id, elapsed_ms }, usage = { input, output }, items, turns }`, or nil. A chat with no messages yet has `new = true` and no `session_id`.
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
- `bone.api.log(n)` (the last n messages), `bone.api.show(text)` (show without logging), `bone.api.colors_name()`, `bone.api.exec_lua(code)` and `bone.api.source(path)` (what `/lua` and `/source` do)
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
- `bone.ui.sessions()`: the session picker (`ctrl+r`, `/sessions`), defined in `runtime/tui/defaults.lua` with `bone.ui.select`. Replace it to change how sessions are listed.
- `bone.ui.suggestions(ctx)` → lines: draws the `/` menu (see Commands) right above the prompt. `ctx = { items = { { name, desc } }, selected, width, height }`. Set it to `nil` for no list (tab still completes).
- `bone.api.open_session(id)`: show a session.
- `bone.ui.pager(content, { title, width, height })` → handle: scrollable text in a focused window. `content` is a string (wrapped; `#` lines are headings) or a list of lines. `up`/`down`/wheel, `pageup`/`pagedown`, `home`/`end`, `esc` or `q` closes. `handle:set(content)`, `handle:close()`.
- `bone.ui.help(topic)` and `bone.ui.health()`: what `/help {topic}` and `/health` call (in `runtime/tui/defaults.lua`). The docs are in `bone._docs`; the TUI's built-in checks come from `bone.api.health()`.
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

### Colors

Everything is drawn with named highlight groups. `/colorscheme black` (the default, the original bone palette) and `/colorscheme ansi` (16 terminal colors) ship with bone; `colors/<name>.lua` in `~/.bone/` or a plugin adds more.

```lua
bone.colorscheme("ansi")
bone.hl.set("ToolPath", { fg = "#7dcfff", bold = true })  -- fg/bg: #rrggbb, a name, or 0-255
bone.hl.set("MyGroup", { link = "ToolError", underline = true })
bone.hl.get("ToolPath")   -- { fg = "#7dcfff", bold = true }
bone.hl.names()
```

`/hi Group fg=#ff0000 bold` sets one from the prompt; `/hi` lists the groups.

Groups: `Normal Dim Accent UserPrompt UserMessage Reasoning ToolName ToolArgs ToolPath ToolSummary ToolOutput ToolGutter ToolRunning ToolError DiffAdd DiffDelete ShellProgram ShellPath ShellFlag ShellString ShellVariable ShellComment ShellOperator MdHeading MdBold MdItalic MdCode MdCodeBlock MdQuote MdBullet MdLink Notice ErrorMsg WarningMsg WinSeparator StatusLine StatusLineDim Selection Placeholder PopupBorder PopupTitle PanelTitle PanelTitleFocus`.

### Drawing the screen

Rust keeps the session data, wraps text, caches, scrolls and paints. Everything about how things look is Lua, and **by default there is none**: the screen is the chat as plain text (`> ` before your messages, one blank line between items, tool calls as name, arguments and output, reasoning hidden) and the prompt below it. Message text reaches views without blank lines at its edges. No statusline, prompt prefix, colors or spacing until Lua adds them; the one default is a blank divider row between the chat and the prompt (`bone.ui.divider` in `runtime/tui/defaults.lua`; set it to `nil` to remove it).

`examples/plugins/style/` is a complete look built only from this API (views, tool calls, statusline, divider, prompt prefix, the empty-session hint). Install it with `cp -r examples/plugins/style ~/.bone/plugins/`, or copy the parts you want.

#### The prompt

```lua
bone.ui.prompt = {
  prefix = { { "› ", "UserPrompt" } },                 -- a line; following rows are indented to match
  placeholder = { { "Message bone…", "Placeholder" } }, -- shown while the prompt is empty
}
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
| `user` | `text` |
| `reasoning`, `assistant` | `text`, `streaming` |
| `tool` | `id`, `name`, `arguments` (decoded), `raw_arguments`, `output` (nil while running), `is_error`, `done` |
| `notice` | `text`, `error` |
| `queued` | `id`, `text`, `mode` (`"steer"` joins the running turn, `"next"` waits for its own), `position` (1 first); messages waiting in the session's queue, after the transcript. Plain text: `(queued) text` |

Every item also has `kind` and `index`. `ctx` is `{ width, region, prev = { kind } }`: `region` is `"chat"` in the transcript, and `prev` lets a view decide spacing (the style plugin adds a blank line except between a message and its tool calls). Assigning a view redraws the chat; a view that errors is reported once and its items fall back to plain text until it is redefined. Call `bone.ui.refresh()` if a view depends on something else you changed.

Tool calls have one more layer: `views.tool` draws the frame (status marker, header, gutter), and `bone.ui.tool_views[name](item, ctx)` can supply the content for one tool as `{ title = line, lines = { line, ... } }`. With the style plugin, return `nil` for its content (`bone.ui.tool_content`).

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

Regions `left` and `right` sit beside the chat (sized in columns). Every other region is a row band placed by `bone.ui.layout`, which lists the screen's rows from top to bottom:

```lua
bone.ui.layout = { "top", "chat", "divider", "above_prompt", "prompt", "statusline" }  -- the default
bone.ui.layout = { "statusline", "chat", "mybar", "prompt" }  -- statusline on top, a custom band
bone.ui.regions.mybar = { size = 1, render = function(ctx) return { "hello" } end }
```

`chat`, `prompt`, `divider` and `statusline` are built in; leave one out and it is not shown. Row regions are sized in rows. `size` is a number or `"auto"` (fit the content, up to `max`, default 10); a bare function means `size = "auto"`. `render(ctx)` gets `{ region, width, height, spinner, popup, session }` and returns lines, or `nil` to hide the region. Regions are redrawn every frame, so keep them light.

- `bone.chat.items({ kind = "tool", last = 3 })` returns items of the session on screen, as views receive them (more filters under Chat data).
- `bone.ui.render(item, width, region)` renders an item with the current views.

#### Panels: docked areas you own

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
- Placement: `left` and `right` panels take full-height columns (after any `left`/`right` regions), then `top` and `bottom` panels split the chat column. Among panels on the same side, lower `order` (default 0) is placed nearer the edge, then older first. Side panels get a `WinSeparator` bar next to the chat.
- Content: `render(ctx)` returns every line (or nil for none), with `ctx = { id, dock, width, height, focused, top, title }`; for `"auto"` it gets the most room it could have. `lines` is a fixed list instead (a table you keep changing is fine, it is read every frame). Rust shows the rows from `top`; `follow = true` keeps the end in view as content grows, until it is scrolled away from it. A render error is reported once and leaves the panel empty until `update`/`set_lines` gives it new content.
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
- `bone.ui.markdown(text, width, indent)` → the style plugin's rendering of that, as lines (only with the plugin).
- `bone.text.wrap(spans, width, { first, rest, pad })` → lines. `first`/`rest` prefix the first and following rows (`rest` defaults to blanks as wide as `first`); `pad = "Group"` fills each row to the width (background bands).
- `bone.text.clip(spans, width)`, `bone.text.truncate(s, n)`, `bone.text.width(s)`.
- `bone.text.shell(cmd)` → the command as highlighted spans.
- `bone.ui.preview(text, max, group)` (style plugin) → at most `max` rows: first line, `⋮ +N lines`, last lines.

Errors in Lua never crash bone: they show as a one-line message, with the full traceback in `/messages`.
