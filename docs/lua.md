# Lua in bone

bone runs two separate LuaJIT states, one per side:

| File | Runs in | Controls |
|---|---|---|
| `~/.bone/core.lua` | the core (server) | providers, system prompt, tools, hooks |
| `~/.bone/tui.lua` | the TUI | options, keys, commands, colors, events |

`$BONE_CONFIG_DIR` replaces `~/.bone`. The two states never share Lua values; they talk only through the bone protocol. Lua has full trust (no sandbox). Tool calls run without asking; asking first is a plugin (`examples/plugins/approve`, see below).

Load order on each side: `runtime/<side>/api.lua`, then `runtime/<side>/defaults.lua`, then your file. The runtime is built into the binary, and any runtime file can be replaced by putting a file at the same path under `~/.bone/runtime/` (for example `~/.bone/runtime/tui/defaults.lua` drops every default keymap).

`require("x.y")` finds `~/.bone/lua/x/y.lua` (or `x/y/init.lua`), then each plugin's `lua/`, then the runtime's `lua/` modules.

## Plugins

A plugin is a folder in `~/.bone/plugins/`:

```text
~/.bone/plugins/git/
  core.lua     runs in the core (tools, hooks)
  tui.lua      runs in the TUI (keys, commands, tool views)
  lua/         modules for require()
  colors/      colorschemes
```

Every part is optional. Plugins load in name order, after the runtime defaults and before your own `core.lua` / `tui.lua`, so your config can change anything a plugin set up. Rename a folder to start with `_` or `.` to disable it. `bone.plugins` lists the loaded names. Installing is just copying or `git clone`-ing into `~/.bone/plugins/`; see `examples/plugins/` in the repo (`git`, `approve`, `style`).

## Shared

- `bone.side`: `"core"` or `"tui"`. `bone.version`, `bone.config_dir`.
- `bone.json.encode(value)`, `bone.json.decode(string)`
- `bone.inspect(value)`: readable dump of any value.
- `bone.util`: `split(s, sep)`, `trim(s)`, `startswith(s, prefix)`, `extend(t1, t2, ...)`.

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
```

The working directory is always appended to the system prompt.

### Tools

```lua
bone.tool.register {
  name = "git_log",
  description = "Show recent commits.",
  parameters = { type = "object", properties = { n = { type = "integer" } } },  -- JSON Schema
  needs_approval = false,   -- read by the approve plugin (default: ask)
  run = function(args, ctx)  -- ctx = { cwd, session_id }
    local r = bone.system("git log --oneline -n " .. (args.n or 10), { cwd = ctx.cwd })
    if r.code ~= 0 then return nil, r.stderr end   -- an error result
    return r.stdout                                -- or a table (sent as JSON)
  end,
}
```

A Lua tool with the same name as a built-in (`read_file`, `write_file`, `edit_file`, `shell`) replaces it. `error()` inside `run` becomes an error result for the model.

- `print(...)` appends to `~/.bone/core.log` (the core may share the terminal with the TUI).

### Hooks

A hook runs at a point in the core with an event table. It returns `nil` (no change), a table of fields to change (later hooks and the core see them), or `{ deny = "why" }` to stop that step. Hooks run in registration order: plugins, then `core.lua`. A hook that errors counts as a refusal.

| Point | Event | `deny` means |
|---|---|---|
| `turn_start` | `{ session_id, cwd, text }`, before the user's message is saved | the turn fails with "why" |
| `request` | `{ session_id, messages, tools }`, before each call to the model | the turn fails |
| `message` | `{ session_id, content, reasoning, tool_calls }`, the model's reply before it is saved | the turn fails |
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

### Environment overrides

For one-off runs, these override `core.lua`: `BONE_BASE_URL` + `BONE_MODEL` (use this endpoint), `BONE_MODEL` alone (another model on the configured provider), `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, `BONE_DATA_DIR`, and `BONE_APPROVAL=auto` (the approve plugin, if installed, never asks).

## TUI (`tui.lua`)

The TUI has no modes: keys go to the prompt, and while a focused window is open (a popup, the session picker, any `bone.ui.select`) its own keys apply first. The keymap *contexts* are `main` and `popup` (keys a focused window doesn't handle itself).

### Keys

```lua
bone.keymap.set("f2", "/sessions")                            -- a slash command
bone.keymap.set("ctrl+l", "new_session")                      -- a builtin action
bone.keymap.set("alt+s", function() bone.cmd("set noshow_reasoning") end)
bone.keymap.set("ctrl+q", "interrupt", { context = "popup" })  -- while a popup is open
bone.keymap.del("ctrl+n")
```

Key names: `ctrl+`, `alt+` and `shift+` combined with `enter`, `esc`, `tab`, `backspace`, `delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `space`, `f1`–`f24`, `wheelup`, `wheeldown` (the mouse wheel) or a single character (`"?"`, `"G"`). Keys without a mapping type text.

Builtin actions: `submit newline left right up down word_left word_right line_start line_end backspace delete delete_word delete_to_start delete_to_end scroll_up scroll_down page_up page_down scroll_top scroll_bottom complete dismiss interrupt quit quit_if_empty new_session sessions`.

The defaults are in `runtime/tui/defaults.lua`.

### Options

```lua
bone.o.show_reasoning = false
bone.o.tool_preview_lines = 4  -- rows of tool output under each call
bone.o.diff_preview_lines = 8  -- rows of diff under each edit
bone.o.prompt_max_height = 10
bone.o.mouse = false           -- leave the mouse to the terminal (its own selection, no wheel)
```

### Commands

```lua
bone.cmd("sessions")                      -- run any slash command (the / is optional)
bone.cmd.create("hello", function(c)      -- lowercase letters, digits, - and _
  bone.notify("hi " .. c.args)
end, { desc = "say hi" })                 -- /hello world
bone.cmd.del("hello")
```

User commands appear in the `/` suggestions with their `desc`, and in `/help`. They can't replace built-in commands. Built-in commands related to Lua: `/lua code`, `/lua =expr` (show a value), `/source file`, `/messages` (recent messages, full Lua errors).

### Events

```lua
local id = bone.on("turn/finished", function(ev) ... end)
bone.off(id)
```

- Every server notification, by method, with its params: `turn/started`, `message/delta`, `message/completed`, `tool/started`, `tool/finished`, `ask/requested`, `ask/resolved`, `turn/finished`.
- `ready`: after `tui.lua` has run.
- `submit`: `{ text }` before a message is sent. Return `false` to cancel, or a string to send instead.

### Talking to the core

```lua
bone.request("session/list", {}, function(result, err)
  if err then return bone.notify(err, "error") end
  bone.notify(#result .. " sessions")
end)
```

Any protocol method works (see `crates/bone-proto/src/methods.rs`).

### UI API

- `bone.system(cmd, { cwd, stdin, timeout }, function(r, err) ... end)`: run a command in the background; the callback gets `{ code, stdout, stderr }` (or `{ timed_out = true }`), or `nil, err`. The UI never waits.
- `bone.http(req, function(res, err) ... end)`: an HTTP request in the background (same fields as the core's).
- `bone.defer(ms, fn)`: run `fn` later.
- `bone.notify(msg, level)`: `level` is `"info"` (default) or `"error"`. `print(...)` is `bone.notify`.
- `bone.press("ctrl+c")`: press a key. `bone.action("scroll_top")`: run a builtin action.
- `bone.api.prompt_get()`, `bone.api.prompt_set(text)`
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
- `bone.ui.suggestions(ctx)` → lines: draws the matching commands while you type `/…`, right above the prompt. `ctx = { items = { { name, desc } }, selected, width, height }`; Rust keeps the matching, `up`/`down` selection and `tab` completion. Set it to `nil` for no list.
- `bone.api.open_session(id)`: show a session.
- `bone.ui.box(lines, { title, title_hl, border_hl, width, pad, chars })` → the lines inside a rounded border (`PopupBorder`/`PopupTitle` by default). Only used if you call it.

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

Groups: `Normal Dim Accent UserPrompt UserMessage Reasoning ToolName ToolArgs ToolPath ToolSummary ToolOutput ToolGutter ToolRunning ToolError DiffAdd DiffDelete ShellProgram ShellPath ShellFlag ShellString ShellVariable ShellComment ShellOperator MdHeading MdBold MdItalic MdCode MdCodeBlock MdQuote MdBullet MdLink Notice ErrorMsg WarningMsg WinSeparator StatusLine StatusLineDim Selection Placeholder PopupBorder PopupTitle`.

### Drawing the screen

Rust keeps the session data, wraps text, caches, scrolls and paints. Everything about how things look is Lua, and **by default there is none**: the screen is the chat as plain text (`> ` before your messages, one blank line between items, tool calls as name, arguments and output, reasoning hidden) and the prompt below it. Message text reaches views without blank lines at its edges. No statusline, divider, prompt prefix, colors or spacing until Lua adds them.

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

- `bone.chat.items({ kind = "tool", last = 3 })` returns items of the session on screen, as views receive them.
- `bone.ui.render(item, width, region)` renders an item with the current views.

#### Statusline and divider

```lua
bone.ui.statusline = function(ctx)
  return { { " " .. ctx.title, "StatusLine" }, "%=", { ctx.spinner, "Accent" }, " " }
end
bone.ui.divider = function(ctx)   -- the line between the chat and the prompt
  return { { fill = "─", hl = "WinSeparator" } }
end
```

Each returns one line (`"%="` is a blank stretch); the row exists only while the function is defined. The statusline context has `title`, `popup` (`"popup"` while a focused window is open, else nil), `spinner`, `width` and `session` (`{ title, cwd, running, elapsed, usage = { input, output } }` or nil); the divider context has `spinner`, `width` and `session`. If one errors, its row is blank until it is redefined.

#### Helpers

- `bone.markdown.parse(text)` → blocks: `{ kind = "heading", level, spans }`, `"paragraph"` / `"item"` (`indent`, and for items `marker`, `ordered`), `"quote"`, `"code"` (`lang`, `lines`), `"rule"`, `"blank"`. Spans are `{ text, bold?, italic?, code?, link? }`.
- `bone.ui.markdown(text, width, indent)` → the style plugin's rendering of that, as lines (only with the plugin).
- `bone.text.wrap(spans, width, { first, rest, pad })` → lines. `first`/`rest` prefix the first and following rows (`rest` defaults to blanks as wide as `first`); `pad = "Group"` fills each row to the width (background bands).
- `bone.text.clip(spans, width)`, `bone.text.truncate(s, n)`, `bone.text.width(s)`.
- `bone.text.shell(cmd)` → the command as highlighted spans.
- `bone.ui.preview(text, max, group)` (style plugin) → at most `max` rows: first line, `⋮ +N lines`, last lines.

Errors in Lua never crash bone: they show as a one-line message, with the full traceback in `/messages`.
