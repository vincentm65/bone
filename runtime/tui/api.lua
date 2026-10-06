-- The TUI Lua API. Thin wrappers over `bone._api`, which runs each operation
-- directly against the UI. Loaded before runtime/tui/defaults.lua and
-- ~/.bone/tui.lua.

local api = bone._api

bone.keymap = {}

--- Map a key or whitespace-separated key sequence (`"g g"`, `"ctrl+x enter"`) to an
--- action: a builtin name (`"submit"`, `"page_up"`, ...), a slash command
--- (`"/sessions"`), or a Lua function.
--- opts.context: `"main"` (default), `"popup"`, or a named context. A focused
--- Lua window gets its own keys first, then the `popup` context. Unmapped text
--- in `main` and named contexts enters the prompt.
function bone.keymap.set(key, action, opts)
  api("keymap_set", key, action, opts or {})
end

--- Define or update a named context. `opts.fallback` is a context name or
--- list of names (default: `main`); `opts.priority` orders fallback lookup.
function bone.keymap.context(name, opts)
  api("keymap_context", name, opts or {})
end
bone.keymap.define = bone.keymap.context
bone.keymap.set_context = bone.keymap.context

--- Focus a named context. Pass nil, or call clear(), to return to `main`.
function bone.keymap.focus(name)
  api("keymap_focus", name)
end
function bone.keymap.clear()
  api("keymap_clear")
end
function bone.keymap.current()
  return api("keymap_current")
end

function bone.keymap.del_context(name)
  api("keymap_context_del", name)
end
bone.keymap.delete_context = bone.keymap.del_context

--- Intercept raw key names before keymaps. Return true to consume; any other
--- return value passes the key to normal popup/keymap handling.
function bone.keymap.raw(fn, opts)
  return api("keymap_raw", fn, opts or {})
end
bone.keymap.intercept = bone.keymap.raw
function bone.keymap.raw_del(id)
  return api("keymap_raw_del", id)
end
bone.keymap.remove_raw = bone.keymap.raw_del

function bone.keymap.del(key, opts)
  api("keymap_del", key, opts or {})
end

--- Run a slash command: bone.cmd("/sessions") or bone.cmd("new").
bone.cmd = setmetatable({}, {
  __call = function(_, line)
    api("cmd", line)
  end,
})

--- Define /name. Names are lowercase letters, digits, - and _.
--- fn receives { args = "...", argv = { ... } }. With opts.args, it also
--- receives typed `arguments` (and the same table as `parsed`). opts.complete
--- returns strings or { value, desc } entries for argument completion.
function bone.cmd.create(name, fn, opts)
  api("command_create", name, fn, opts or {})
end

function bone.cmd.del(name)
  api("command_del", name)
end

--- Every command: { { name, desc, aliases, complete } }, sorted by name.
function bone.cmd.list()
  return api("command_list")
end

--- The command a name or alias refers to, or nil.
function bone.cmd.find(word)
  return api("command_find", word)
end

--- Run a command's completion function with ctx = { command, text, args,
--- token, argv }; nil when it has none.
function bone.cmd.complete(name, ctx)
  return api("command_complete", name, ctx)
end

--- Options: bone.o.show_reasoning = false; print(bone.o.tool_preview_lines)
--- Dynamic options: bone.o.define("name", default, { type, desc, on_change }),
--- bone.o.del("name"), bone.o.names(), and bone.o.info("name").
local option_api = {}
function option_api.get(name)
  return api("opt_get", name)
end
function option_api.define(name, default, opts)
  api("opt_define", name, default, opts or {})
end
function option_api.del(name)
  return api("opt_del", name)
end
option_api.delete = option_api.del
function option_api.names()
  return api("opt_names")
end
function option_api.info(name)
  return api("opt_info", name)
end
--- One option argument: "name", "noname", "name!", "name=value", "name?".
--- Returns what to show (for "name?"), or nil; errors on a bad argument.
function option_api.apply(arg)
  return api("opt_apply", arg)
end
--- Every option as "name=value".
function option_api.list()
  return api("opt_list")
end
bone.o = setmetatable(option_api, {
  __index = function(_, name)
    return api("opt_get", name)
  end,
  __newindex = function(_, name, value)
    api("opt_set", name, value)
  end,
})

--- Run fn(data) on an event. Events: every server notification by method
--- ("turn/started", "message/delta", "tool/finished", ...) plus "ready" and
--- "submit" ({ text }; return false to cancel or a string to replace the
--- text). Returns an id for bone.off.
function bone.on(event, fn)
  return api("on", event, fn)
end

function bone.off(id)
  api("off", id)
end

--- Call the core API: bone.request("session/list", {}, function(result, err) end)
function bone.request(method, params, callback)
  api("request", method, params or {}, callback)
end

--- The saved settings (settings.json in the config dir, kept by the core):
---   bone.settings.get("tui.tool_detail")   -- a value, or nil
---   bone.settings.all()                    -- every saved setting
---   bone.settings.set(path, value, cb)     -- save it for good; cb(all, err)
---   bone.settings.reset(path, cb)          -- remove it (back to the default)
--- Paths are dotted: "provider", "models.<provider>", "tui.<option>" (applied
--- before tui.lua, which runs last), or a plugin's own "<plugin>.<key>".
--- Every client hears the change as the `settings/changed` event.
bone.settings = {
  all = function()
    return api("settings_get")
  end,
  get = function(path)
    local at = api("settings_get")
    for key in tostring(path):gmatch("[^.]+") do
      if type(at) ~= "table" then
        return nil
      end
      at = at[key]
    end
    return at
  end,
  set = function(path, value, callback)
    bone.request("settings/set", { path = path, value = value, session_id = (bone.chat.session() or {}).session_id }, callback)
  end,
  reset = function(path, callback)
    bone.request("settings/reset", { path = path }, callback)
  end,
  --- A tab of your own in /config: { name, title, fields = { { key, label,
  --- type = "boolean" | "number" | "integer" | "string", choices, default,
  --- min, max, desc } } }. Values are saved as "<name>.<key>". A plugin can
  --- declare the same as "settings" in its manifest.json instead.
  page = function(spec)
    assert(type(spec) == "table" and type(spec.name) == "string" and type(spec.fields) == "table",
      "bone.settings.page{ name, fields }")
    spec.title = spec.title or spec.name
    bone._settings_pages = bone._settings_pages or {}
    for i, p in ipairs(bone._settings_pages) do
      if p.name == spec.name then
        bone._settings_pages[i] = spec
        return
      end
    end
    bone._settings_pages[#bone._settings_pages + 1] = spec
  end,
}

--- Call a model through the core (no session, no tools run):
---   bone.model.complete(req, on_delta, on_done) -> handle
---     req: { provider, messages or prompt (+ system), tools, options }
---     on_delta({ text = ... } or { reasoning = ... }) as it streams (may be nil)
---     on_done(result, err): result = { content, reasoning, tool_calls, usage }
---   handle:cancel()
---   bone.model.list(function(list, err) ... end): { name, model, type, current }
-- Events can arrive before the reply that says which call they belong to;
-- they wait here until it does.
local model_calls, model_done_early, model_deltas_early = {}, {}, {}

local function model_finish(call, ev)
  if ev.error then
    call.on_done(nil, ev.error)
  else
    local m = ev.message or {}
    call.on_done({ content = m.content or "", reasoning = m.reasoning or "", tool_calls = m.tool_calls or {}, usage = ev.usage })
  end
end

bone.model = {
  complete = function(req, on_delta, on_done)
    assert(type(req) == "table", "bone.model.complete(req, on_delta, on_done)")
    on_done = on_done or function() end
    local messages = req.messages
    if not messages then
      assert(type(req.prompt) == "string", "a model call needs messages or a prompt")
      messages = {}
      if req.system then
        messages[1] = { role = "system", content = req.system }
      end
      messages[#messages + 1] = { role = "user", content = req.prompt }
    end
    local handle = { id = nil, cancelled = false }
    function handle:cancel()
      self.cancelled = true
      if self.id then
        bone.request("model/cancel", { request_id = self.id })
      end
    end
    bone.request("model/complete", {
      provider = req.provider,
      messages = messages,
      tools = req.tools,
      options = req.options,
      stream = on_delta ~= nil,
    }, function(r, err)
      if err then
        return on_done(nil, err)
      end
      handle.id = r.request_id
      local call = { on_delta = on_delta, on_done = on_done }
      for _, d in ipairs(model_deltas_early[r.request_id] or {}) do
        if on_delta then
          on_delta(d)
        end
      end
      model_deltas_early[r.request_id] = nil
      local early = model_done_early[r.request_id]
      if early then
        model_done_early[r.request_id] = nil
        return model_finish(call, early)
      end
      model_calls[r.request_id] = call
      if handle.cancelled then
        bone.request("model/cancel", { request_id = r.request_id })
      end
    end)
    return handle
  end,
  list = function(callback)
    bone.request("model/list", { session_id = (bone.chat.session() or {}).session_id }, callback)
  end,
}

bone.on("model/delta", function(ev)
  local call = model_calls[ev.request_id]
  if call then
    if call.on_delta then
      call.on_delta({ [ev.kind] = ev.text })
    end
  else
    local early = model_deltas_early[ev.request_id] or {}
    early[#early + 1] = { [ev.kind] = ev.text }
    model_deltas_early[ev.request_id] = early
  end
end)

bone.on("model/completed", function(ev)
  local call = model_calls[ev.request_id]
  if call then
    model_calls[ev.request_id] = nil
    model_finish(call, ev)
  else
    model_done_early[ev.request_id] = ev
  end
end)

-- Another client's model calls never get a reply here; forget their events
-- after a while.
bone.on("model/completed", function(ev)
  bone.defer(60000, function()
    model_done_early[ev.request_id] = nil
    model_deltas_early[ev.request_id] = nil
  end)
end)

--- Call a function core Lua registered with bone.rpc.register (lua/call):
---   bone.rpc.call("myplugin.do", { ... }, function(result, err) end)
--- It runs for the session on screen (ctx.session_id, ctx.cwd in the core).
bone.rpc = {
  call = function(name, args, callback)
    local s = bone.chat.session()
    bone.request("lua/call", {
      name = name,
      args = args,
      session_id = s and s.session_id or nil,
      cwd = s and s.cwd or nil,
    }, callback)
  end,
}

--- Show a message. level: "info" (default) or "error".
--- Run a shell command in the background. on_exit(result) runs when it
--- ends with { code, stdout, stderr } (or { timed_out = true }), or
--- (nil, error). opts: { cwd, stdin, timeout = ms }. The UI never waits.
---   bone.system("git status --short", { cwd = dir }, function(r) ... end)
function bone.system(cmd, opts, on_exit)
  if type(opts) == "function" then
    opts, on_exit = nil, opts
  end
  opts = opts or {}
  api("wait", { system = cmd, cwd = opts.cwd, stdin = opts.stdin, timeout = opts.timeout }, on_exit)
end

--- An HTTP request in the background: { url, method, headers, body,
--- timeout = ms }; callback(res) with { status, headers, body }, or
--- (nil, error).
function bone.http(req, callback)
  api("wait", { http = req }, callback)
end

--- A streaming job: a process with a handle. Output reaches the callbacks as
--- it arrives; the UI never waits.
---   cmd: a shell command (bash -c) or an argv list { "git", "status" }
---   opts: { name, cwd, env = { K = "v" }, stdin = "text" or true (keep it
---     open for job:write), timeout = ms, lines = true (whole lines instead
---     of chunks), buffer (keep output for on_exit; default when there are
---     no output callbacks), on_stdout(data, job), on_stderr(data, job),
---     on_exit(result, job) }
--- result: job:status() plus { cancelled, timed_out, duration_ms } and,
--- when buffering, { stdout, stderr, truncated }.
--- Returns a handle: job.id, job:cancel(), job:write(data),
--- job:close_stdin(), job:status(), job:running().
local Job = {}
Job.__index = Job

local function job_handle(id)
  return setmetatable({ id = id }, Job)
end

function Job:cancel()
  return api("job_cancel", self.id)
end
function Job:write(data)
  return api("job_write", self.id, data)
end
function Job:close_stdin()
  return api("job_close_stdin", self.id)
end
--- { id, name, cmd, pid, state, running, elapsed_ms, stdout_bytes,
--- stderr_bytes, code, signal, error }, or nil once forgotten.
function Job:status()
  return api("job_status", self.id)
end
function Job:running()
  local s = api("job_status", self.id)
  return s ~= nil and s.running
end

bone.job = {
  _handle = job_handle,
  start = function(cmd, opts)
    return job_handle(api("job_start", cmd, opts or {}))
  end,
  --- The handle of a known job, or nil.
  get = function(id)
    if api("job_status", id) then
      return job_handle(id)
    end
  end,
  --- Running jobs and the last few finished ones, oldest first.
  list = function()
    return api("job_list")
  end,
  --- Cancel every running job.
  cancel_all = function()
    for _, s in ipairs(api("job_list")) do
      if s.running then
        api("job_cancel", s.id)
      end
    end
  end,
}

--- Core-managed shell processes started by the model (of the session on
--- screen). Entries: id, command, state, running, pid, started_at_ms,
--- finished_at_ms, elapsed_ms, tail (the last line of output), code,
--- signal, error, terminal (it runs in a pseudo-terminal).
---   bone.processes.screen(id, { width, height, scroll }) -> { lines, scroll,
---       max }: the process's terminal at that size, scrolled back `scroll`
---       rows (at most `max`), as lines of { text, group }. The first call
---       reads its output, which shows a moment later; a running process's
---       terminal is resized to match.
bone.processes = {
  screen = function(id, opts)
    return api("process_screen", id, opts or {})
  end,
  list = function()
    return api("process_list")
  end,
  refresh = function()
    api("process_refresh")
  end,
  cancel = function(id)
    return api("process_cancel", id)
  end,
}

--- Run fn after `ms` milliseconds.
function bone.defer(ms, fn)
  api("wait", { sleep = ms }, function()
    fn()
  end)
end

function bone.notify(msg, level)
  api("notify", tostring(msg), level or "info")
end

--- Press a key as if typed: bone.press("ctrl+c")
function bone.press(key)
  api("press", key)
end

--- Run a builtin action by name.
function bone.action(name)
  api("action", name)
end

bone.api = {
  --- The colorscheme loaded with bone.colorscheme, or nil.
  colors_name = function()
    return api("colors_name")
  end,
  --- The last n messages shown.
  log = function(n)
    return api("log_tail", n or 20)
  end,
  --- Show text in the message area without adding it to the log.
  show = function(text)
    api("show_message", text)
  end,
  --- Run Lua ("=expr" shows a value); source runs a file.
  exec_lua = function(code)
    api("exec_lua", code)
  end,
  source = function(path)
    api("source_file", path)
  end,
  prompt_get = function()
    return api("prompt_get")
  end,
  prompt_set = function(text)
    api("prompt_set", text)
  end,
  --- The session on screen, or nil: { session_id, cwd, title, running }
  --- The TUI's own checks for /health: a list of { name, status, message }.
  health = function()
    return api("health")
  end,
  --- Show a session (loading it if needed).
  open_session = function(id)
    api("open_session", id)
  end,
  session = function()
    return api("session")
  end,
}

--- Highlight groups: bone.hl.set("ToolPath", { fg = "#7dcfff", bold = true })
--- Keys: fg, bg (#rrggbb, a color name, or 0-255), bold, italic, underline,
--- reverse, dim, link (start from another group).
bone.hl = {
  set = function(name, spec)
    api("hl_set", name, spec or {})
  end,
  get = function(name)
    return api("hl_get", name)
  end,
  --- Back to the built-in 16-color styles.
  reset = function()
    api("hl_reset")
  end,
  names = function()
    return api("hl_names")
  end,
}

--- Load colors/<name>.lua from ~/.bone, a plugin, or the runtime.
function bone.colorscheme(name)
  api("colorscheme", name)
end

--- How the screen is drawn, all in Lua. The defaults draw the standard UI
--- (runtime/lua/bone/ui/); with nothing defined Rust draws plain text. Lines are lists of { "text", "Group" } items (or
--- plain strings; { fill = "─", hl = "Group" } stretches).
---   bone.ui.views[kind] = function(item, ctx) return lines end
---       kind: "user", "reasoning", "assistant", "tool", "notice".
---       ctx = { width, region, prev = { kind } }. Return nil to hide.
---   bone.ui.tool_views[name] = function(item, ctx) return { title, lines } end
---       the content of one tool's calls; views.tool frames it.
---   bone.ui.regions[name] = { size = n | "auto", max = n, render = function(ctx) return lines end }
---       name: "top", "above_prompt" (rows), "left", "right" (columns).
---   bone.ui.statusline(ctx), bone.ui.divider(ctx): one row each, only
---       while defined.
---   bone.ui.layout = { "top", "chat", "divider", "above_prompt", "prompt",
---       "statusline" }: the rows, top to bottom (this is the default); any
---       other name is a region.
---   bone.ui.prompt = { prefix = line, placeholder = line }
---
--- Changing views or tool_views redraws the chat. Call bone.ui.refresh()
--- after changing something they depend on (e.g. your own settings).

-- A table that redraws when a key is assigned.
local function watched()
  local store = {}
  return setmetatable({}, {
    __index = store,
    __newindex = function(_, k, v)
      store[k] = v
      api("ui_refresh")
    end,
  }), store
end

local views, view_store = watched()
local tool_views, tool_view_store = watched()
bone.ui = { views = views, tool_views = tool_views, regions = {} }

--- Remove every view, tool view, region, action and the statusline,
--- divider, prompt and layout: a blank screen to build on (Rust draws plain
--- text until you define things again).
function bone.ui.clear()
  bone.ui.statusline = nil
  bone.ui.divider = nil
  bone.ui.prompt = nil
  bone.ui.layout = nil
  for k in pairs(view_store) do view_store[k] = nil end
  for k in pairs(tool_view_store) do tool_view_store[k] = nil end
  for k in pairs(bone.ui.regions) do bone.ui.regions[k] = nil end
  for k in pairs(bone.ui.actions) do bone.ui.actions[k] = nil end
end

--- Lua handlers for builtin actions: bone.ui.actions[name] = function()
--- return true end handles the action; returning anything else lets the
--- built-in run. The / command menu (runtime/lua/bone/menu.lua) handles
--- submit, complete, dismiss, up and down this way.
bone.ui.actions = {}

--- Draw every chat item again: for views that depend on your own state.
--- Costly in a long chat, and not needed for regions, the statusline or
--- popups, which are drawn on every frame (after any key, event or callback).
function bone.ui.refresh()
  api("ui_refresh")
end

--- Render an item with the current views, e.g. inside a region.
function bone.ui.render(item, width, region)
  local view = bone.ui.views[item.kind]
  if not view then
    return { item.text or "" }
  end
  return view(item, { width = width, region = region or "region", prev = nil }) or {}
end

--- Open a window over the screen. Lua draws every cell of it (Rust only
--- places it and clears behind it):
---   lines: a list of lines, or function(ctx) -> lines, ctx = { width, height }
---          (the most room it can have); a function is called every frame
---   width, height: size in cells; default: fit the lines (set width if a
---          line uses { fill = ... })
---   anchor: "screen" (default), "chat" (the chat area) or "prompt" (the
---          space above the prompt; the window sits right on the prompt)
---   row, col: position in the anchor; default centered, negative counts
---          from the bottom/right (-1 = touching the edge)
---   z: stacking order (default 0; ties go to the newer window)
---   focus: take the keyboard (default false for win, true for popup)
---   keys: { y = function() ... end, esc = ... }
---   on_key: function(key_name) for other keys; return true if handled
---   guard: ms to ignore keys after opening (so typing can't hit them)
--- Unhandled keys in a focused window go to the "popup" keymaps (ctrl+c
--- cancels the turn). Returns an id. bone.ui.box draws a border if you want.
function bone.ui.win(spec)
  return api("popup_open", spec)
end

--- A window that takes the keyboard (bone.ui.win with focus = true).
function bone.ui.popup(spec)
  if spec.focus == nil then
    spec.focus = true
  end
  return api("popup_open", spec)
end

--- Change fields of an open window (same fields as bone.ui.win; false
--- resets width/height/row/col to automatic). Returns false if it is closed.
function bone.ui.update(id, spec)
  return api("popup_update", id, spec)
end

function bone.ui.close(id)
  api("popup_close", id)
end

function bone.ui.is_open(id)
  return api("popup_is_open", id)
end

--- Panels: persistent areas docked beside, above or below the chat. They
--- take room from the chat, keep their scroll position, can be hidden and
--- shown again, and can take the keyboard. Lua gives all of the content;
--- Rust scrolls it.
---   id: a name (letters, digits, _ - .); default "panel<N>"
---   dock: "right" (default), "left", "top" or "bottom"
---   size: columns (left/right) or rows (top/bottom), a fraction of the
---         room (0.3), or "auto" to fit the content up to `max`
---         (default: 30 columns beside the chat, "auto" above/below it)
---   max: the cap for "auto" (40 columns, 10 rows); min: hide below this
---   order: lower is placed nearer the screen edge (default 0)
---   render: function(ctx) -> lines, ctx = { id, dock, width, height,
---         focused, top, title, spinner }, called every frame; or lines: a list
---   title: a row above the content that does not scroll
---   follow: keep the end in view as content grows
---   focusable (default true), focus: take the keyboard now
---   keys: { enter = function(panel) ... end }, on_key(key, panel): return
---         true if handled; other keys go to the "panel" keymap context
---         (or `context`, a named context)
---   on_close(panel), hidden
--- Returns a handle: panel:update(spec), :set_lines(lines), :close(),
--- :focus(), :hide(), :show(), :toggle(), :scroll(n | "top" | "bottom"),
--- :info(), :is_open().
local Panel = {}
Panel.__index = Panel

local function panel_handle(id)
  return setmetatable({ id = id }, Panel)
end

function Panel:update(spec)
  return api("panel_update", self.id, spec or {})
end
function Panel:set_lines(lines)
  return api("panel_set_lines", self.id, lines)
end
function Panel:close()
  return api("panel_close", self.id)
end
function Panel:focus()
  api("panel_focus", self.id)
end
function Panel:hide()
  return api("panel_update", self.id, { hidden = true })
end
function Panel:show()
  return api("panel_update", self.id, { hidden = false })
end
function Panel:toggle()
  local info = api("panel_info", self.id)
  if not info then
    return false
  end
  return api("panel_update", self.id, { hidden = not info.hidden })
end
function Panel:scroll(to)
  return api("panel_scroll", self.id, to)
end
function Panel:info()
  return api("panel_info", self.id)
end
function Panel:is_open()
  return api("panel_info", self.id) ~= nil
end

bone.ui.panel = setmetatable({
  _handle = panel_handle,
  open = function(spec)
    return panel_handle(api("panel_open", spec))
  end,
  --- The handle of an open panel, or nil.
  get = function(id)
    if api("panel_info", id) then
      return panel_handle(id)
    end
  end,
  --- Every panel's info, in placement order.
  list = function()
    return api("panel_list")
  end,
  update = function(id, spec)
    return api("panel_update", id, spec or {})
  end,
  close = function(id)
    return api("panel_close", id)
  end,
  --- Give a panel the keyboard; nil gives it back to the prompt.
  focus = function(id)
    api("panel_focus", id)
  end,
  --- The id of the panel with the keyboard, or nil.
  focused = function()
    return api("panel_focused")
  end,
}, {
  __call = function(self, spec)
    return self.open(spec)
  end,
})

local function spans(line)
  if type(line) == "string" then
    return { { line, "Normal" } }
  end
  return line or {}
end

--- Put a border around lines (a helper for popups; nothing uses it unless
--- you call it). opts: { title, title_hl, border_hl, width, pad = 1,
--- chars = { "╭", "─", "╮", "│", "╯", "╰" } }. Returns lines.
function bone.ui.box(lines, opts)
  opts = opts or {}
  local c = opts.chars or { "╭", "─", "╮", "│", "╯", "╰" }
  local bhl = opts.border_hl or "PopupBorder"
  local pad = string.rep(" ", opts.pad or 1)
  local inner = 0
  for _, l in ipairs(lines) do
    local w = 0
    for _, sp in ipairs(spans(l)) do
      w = w + bone.text.width(sp[1] or "")
    end
    inner = math.max(inner, w)
  end
  if opts.title then
    inner = math.max(inner, bone.text.width(opts.title) + 2)
  end
  if opts.width then
    inner = opts.width - 2 - 2 * #pad
  end
  inner = math.max(inner, 0)
  local top = { { c[1], bhl } }
  if opts.title then
    local t = bone.text.truncate(" " .. opts.title .. " ", inner + 2 * #pad)
    top[#top + 1] = { t, opts.title_hl or "PopupTitle" }
    top[#top + 1] = { string.rep(c[2], inner + 2 * #pad - bone.text.width(t)), bhl }
  else
    top[#top + 1] = { string.rep(c[2], inner + 2 * #pad), bhl }
  end
  top[#top + 1] = { c[3], bhl }
  local out = { top }
  for _, l in ipairs(lines) do
    local wrapped = bone.text.wrap(spans(l), inner)
    if #wrapped == 0 then
      wrapped = { {} }
    end
    for _, line in ipairs(wrapped) do
      local row = { { c[4] .. pad, bhl } }
      local used = 0
      for _, sp in ipairs(line) do
        row[#row + 1] = sp
        used = used + bone.text.width(sp[1])
      end
      row[#row + 1] = { string.rep(" ", inner - used) .. pad, "Normal" }
      row[#row + 1] = { c[4], bhl }
      out[#out + 1] = row
    end
  end
  out[#out + 1] = { { c[6] .. string.rep(c[2], inner + 2 * #pad) .. c[5], bhl } }
  return out
end

--- The prompt as data. Positions are { row, col } (lines and chars, from
--- 0) or a char offset from 0; anything past the end is clamped. Edits run
--- prompt/changed handlers and never send anything.
bone.prompt = {
  get = function()
    return api("prompt_get")
  end,
  set = function(text)
    api("prompt_set", text)
  end,
  --- { text, lines, cursor, selection = { start, end, text } or nil }
  info = function()
    return api("prompt_info")
  end,
  lines = function()
    return api("prompt_info").lines
  end,
  cursor = function()
    return api("prompt_info").cursor
  end,
  set_cursor = function(pos)
    api("prompt_edit", "cursor", pos)
  end,
  --- Insert at the cursor, replacing the selection.
  insert = function(text)
    api("prompt_edit", "insert", nil, nil, text)
  end,
  --- Remember text for up/down recall (as sending it does).
  history_add = function(text)
    api("history_add", text)
  end,
  get_range = function(from, to)
    return api("prompt_get_range", from, to)
  end,
  --- Replace the text between two positions; the cursor goes after it.
  set_range = function(from, to, text)
    api("prompt_edit", "range", from, to, text)
  end,
  --- Select from `from` to `to` (default: the cursor); the cursor moves to
  --- `to`. Typing replaces the selection, deleting removes it.
  select = function(from, to)
    api("prompt_edit", "select", from, to)
  end,
  --- { start, end, text } or nil.
  selection = function()
    return api("prompt_info").selection
  end,
  clear_selection = function()
    api("prompt_edit", "unselect")
  end,
  offset = function(pos)
    return api("prompt_offset", pos)
  end,
  position = function(offset)
    return api("prompt_position", offset)
  end,
}

--- The chats as read-only data (fresh copies). opts.session picks another
--- open chat by session id; the default is the one on screen.
bone.chat = {}

--- Items as views receive them, plus `turn` (0 before the first message),
--- but tool calls leave out `output`, `live` and `raw_arguments` unless
--- opts.full is true (building them is the costly part of a long chat).
--- opts: { kind, name (tool), turn, running, error, from, to (indexes),
--- first, last (keep N), session, full }
function bone.chat.items(opts)
  return api("chat_items", opts or {})
end

--- Inside a chat view: draw this item again after `ms` milliseconds (a
--- clock, a spinner). Views are otherwise redrawn only when their item, the
--- width, options or views change, or chat data they read changed.
function bone.chat.refresh_in(ms)
  return api("chat_refresh_in", ms)
end

--- Put an item of your own in the chat, after what is there now. It is
--- drawn by bone.ui.views[kind] (or as its `text`), listed by
--- bone.chat.items like any other, and kept until removed or the chat is
--- reloaded; the core never sees it. Returns its id.
---   local id = bone.chat.add("build", { text = "building…", running = true })
---   bone.chat.update(id, { text = "build ok", running = false })
--- opts: { session } to add to another open chat.
function bone.chat.add(kind, fields, opts)
  return api("chat_add", kind, fields or {}, opts)
end

--- Change fields of an item from bone.chat.add (a nil value cannot remove a
--- field; set it to false instead). Returns false if it is gone.
function bone.chat.update(id, fields)
  return api("chat_update", id, fields or {})
end

--- Take an item from bone.chat.add out of the chat. Returns false if gone.
function bone.chat.remove(id)
  return api("chat_remove", id)
end

--- Your copies of built-in runtime files (in the config dir's runtime/):
--- bone.runtime.overrides() → { { path, same }, … } (`same`: identical to
--- the built-in one); bone.runtime.reset(path) moves your copy to
--- runtime-backup/ so the built-in is used again, returning where it went
--- (the TUI reloads by itself when the file goes).
bone.runtime = {
  overrides = function()
    return api("runtime_overrides")
  end,
  reset = function(path, stamp)
    return api("runtime_reset", path, stamp)
  end,
}

--- The chat item at screen cell x, y (0-based): { index, line }, or nil.
function bone.chat.at(x, y)
  return api("chat_at", x, y)
end

--- The chat window: { top, height, rows, follow, first, last }. `top` is the
--- first row shown, `rows` the transcript's height, first/last the indexes
--- of the items on screen.
function bone.chat.view()
  return api("chat_view")
end

--- Scroll so item `index` is at the "top" (default), "center" or "bottom".
function bone.chat.scroll_to(index, at)
  return api("chat_scroll_to", index, at)
end

--- Scroll by `n` rows (negative is up), or to "top" or "bottom" (which
--- follows new output again).
function bone.chat.scroll(n)
  return api("chat_scroll", n)
end

--- Draw the item at `index` again, or every item (no index).
function bone.chat.redraw(index)
  return api("chat_redraw", index)
end

--- The item at `index`, or nil. opts: { session, full } as for bone.chat.items.
function bone.chat.item(index, opts)
  opts = opts or {}
  return api("chat_items", { from = index, to = index, session = opts.session, full = opts.full })[1]
end

function bone.chat.count(opts)
  return #api("chat_items", opts or {})
end

--- Each turn: { index, text, first, last, items, tools, tool_errors,
--- running, outcome ("completed", "cancelled", "failed" or nil), error }.
function bone.chat.turns(opts)
  return api("chat_turns", opts or {})
end

--- { session_id, cwd, created_at, title, new, current, running, starting,
--- turn = { id, elapsed_ms }, usage = { input, output }, items, turns }, or
--- nil when no open chat has that session.
function bone.chat.session(opts)
  return api("chat_session", opts or {})
end

--- Every open chat: { session_id, title, new, current, running }.
function bone.chat.sessions()
  return api("chat_sessions")
end

--- The stored transcript from the core (the authority), as protocol
--- messages: callback(messages, err, result). session_id defaults to the
--- chat on screen; a new chat has none ({}).
function bone.chat.messages(session_id, callback)
  if type(session_id) == "function" then
    session_id, callback = nil, session_id
  end
  if not session_id then
    local s = bone.chat.session()
    session_id = s and s.session_id
  end
  if not session_id then
    callback({}, nil, nil)
    return
  end
  bone.request("session/messages", { session_id = session_id }, function(r, err)
    if err then
      callback(nil, err)
    else
      callback(r.messages, nil, r)
    end
  end)
end

print = function(...)
  local parts = {}
  for i = 1, select("#", ...) do
    parts[#parts + 1] = tostring((select(i, ...)))
  end
  bone.notify(table.concat(parts, " "))
end

-- One UTF-8 character.
local CHAR = "^[%z\1-\127\194-\244][\128-\191]*$"

--- Pick one of `items` in a focused window; typing filters.
---   opts.prompt: text before the filter ("")
---   opts.format: function(item) -> string (default tostring)
---   opts.on_choice: function(item, index), with nil if cancelled
---   opts.loading: show "loading…" until :set_items is called
---   opts.empty: what to show for an empty list
---   opts.footer: the hint line (false for none)
---   opts.width, opts.height: the most room to take (100, 20)
--- Returns a handle: handle:set_items(items), handle:close().
--- Keys: up/down (ctrl+p/n, the wheel) move, enter picks, esc/ctrl+c cancel,
--- backspace/ctrl+u edit the filter, other characters type into it.
function bone.ui.select(items, opts)
  opts = opts or {}
  local format = opts.format or tostring
  local st = { query = "", sel = 1, items = items or {}, loading = opts.loading }
  local handle = {}

  local function matches()
    local q = st.query:lower()
    local out = {}
    for i, it in ipairs(st.items) do
      local text = format(it)
      if q == "" or text:lower():find(q, 1, true) then
        out[#out + 1] = { item = it, index = i, text = text }
      end
    end
    return out
  end

  local id
  local function finish(choice)
    bone.ui.close(id)
    if opts.on_choice then
      if choice then
        opts.on_choice(choice.item, choice.index)
      else
        opts.on_choice(nil)
      end
    end
  end

  local function render(ctx)
    local w = math.max(math.min(opts.width or 100, ctx.width - 4), 10)
    local h = math.max(math.min(opts.height or 20, ctx.height - 2), 5)
    local inner = w - 4
    local list = matches()
    st.sel = math.max(1, math.min(st.sel, #list))
    local body = {
      { { opts.prompt and (opts.prompt .. "  ") or "", "Accent" }, { st.query .. "▏", "Normal" } },
      {},
    }
    local footer = opts.footer == nil and "type to filter · ↑↓ move · enter open · esc close" or opts.footer
    local rows = h - 2 - #body - (footer and 1 or 0)
    if st.loading then
      body[#body + 1] = { { "loading…", "Dim" } }
    elseif #list == 0 then
      body[#body + 1] = { { #st.items == 0 and (opts.empty or "nothing to pick") or "no matches", "Dim" } }
    end
    local first = math.max(1, st.sel - rows + 1)
    for i = first, math.min(#list, first + rows - 1) do
      local t = bone.text.truncate(list[i].text, inner)
      local hl = i == st.sel and "Selection" or "Normal"
      body[#body + 1] = { { t .. string.rep(" ", inner - bone.text.width(t)), hl } }
    end
    while #body < h - 2 - (footer and 1 or 0) do
      body[#body + 1] = {}
    end
    if footer then
      body[#body + 1] = { { footer, "Dim" } }
    end
    return bone.ui.box(body, { width = w })
  end

  local function on_key(k)
    local list = matches()
    if k == "up" or k == "ctrl+p" or k == "wheelup" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "ctrl+n" or k == "wheeldown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif k == "enter" then
      if list[st.sel] then
        finish(list[st.sel])
      end
    elseif k == "esc" or k == "ctrl+c" then
      finish(nil)
    elseif k == "backspace" then
      st.query = st.query:gsub("[%z\1-\127\194-\244][\128-\191]*$", "")
      st.sel = 1
    elseif k == "ctrl+u" then
      st.query, st.sel = "", 1
    elseif k == "space" then
      st.query, st.sel = st.query .. " ", 1
    elseif k:match(CHAR) then
      st.query, st.sel = st.query .. k, 1
    else
      return false
    end
    return true
  end

  id = bone.ui.popup({ lines = render, on_key = on_key })

  function handle:set_items(new)
    st.items, st.loading = new or {}, false
    bone.ui.update(id, {})
  end
  function handle:close()
    bone.ui.close(id)
  end
  return handle
end

--- Plugins: what loaded, their lifecycle, and state that persists.
--- What a plugin creates while its tui.lua or its callbacks run (keymaps,
--- commands, bone.on handlers, panels, windows, jobs, options, raw key
--- interceptors, contexts) is its own and goes away when it unloads.
local open_states = {}

bone.plugin = {
  --- { name, dir, kind = "plugin" | "project", loaded, error } of the
  --- plugin whose code is running, or nil (the runtime, your tui.lua).
  current = function()
    return api("plugin_current")
  end,
  --- Every plugin seen this session, in load order.
  list = function()
    return api("plugin_list")
  end,
  load = function(name)
    api("plugin_load", name)
  end,
  unload = function(name)
    api("plugin_unload", name)
  end,
  reload = function(name)
    api("plugin_reload", name)
  end,
  --- fn() runs when the plugin unloads or reloads, and when bone quits.
  on_shutdown = function(fn)
    assert(type(fn) == "function", "bone.plugin.on_shutdown(fn)")
    api("plugin_on_shutdown", fn)
  end,
  --- A table kept in ~/.bone/state/tui/<name>.json (default: the current
  --- plugin's name). Change it freely: it is saved when the plugin
  --- unloads and when bone quits, or now with bone.plugin.save_state().
  state = function(name)
    local cur = api("plugin_current")
    name = name or (cur and cur.name)
    assert(name, "bone.plugin.state(name): name it outside a plugin")
    if not open_states[name] then
      open_states[name] = { owner = cur and cur.name or false, value = bone.state.load(name) }
    end
    return open_states[name].value
  end,
  save_state = function(name)
    local cur = api("plugin_current")
    name = name or (cur and cur.name)
    local s = name and open_states[name]
    if s then
      bone.state.save(name, s.value)
    end
  end,
}

-- Save open state tables: those of `owner` (forgetting them, it is
-- unloading), or all of them (quitting). Called from Rust.
function bone._save_states(owner)
  for name, s in pairs(open_states) do
    if owner == nil or s.owner == owner then
      local ok, e = pcall(bone.state.save, name, s.value)
      if not ok then
        bone.notify("state " .. name .. ": " .. tostring(e), "error")
      end
      if owner ~= nil then
        open_states[name] = nil
      end
    end
  end
end

--- This directory's project config: { root, file, trusted, loaded } when a
--- .bone/tui.lua is here or above, else nil. It runs only after
--- /plugins trust (remembered per directory).
bone.project = {
  info = function()
    return api("project_info")
  end,
  --- Trust it (and run it now and on later starts here), or not (and
  --- unload it).
  trust = function(on)
    api("project_trust", on ~= false)
  end,
}

bone._health = {}

--- Add a check to /health. fn() returns a status ("ok", "warn", "error",
--- or true/false) and a message.
function bone.health(name, fn)
  assert(type(name) == "string" and type(fn) == "function", "bone.health(name, fn)")
  bone._health[#bone._health + 1] = { name = name, fn = fn }
end

--- Run the TUI checks registered with bone.health.
function bone._run_health()
  local out = {}
  for _, c in ipairs(bone._health) do
    local ok, status, message = pcall(c.fn)
    if not ok then
      status, message = "error", "check failed: " .. tostring(status)
    elseif status == true or status == nil then
      status = "ok"
    elseif status == false then
      status = "error"
    end
    out[#out + 1] = { name = c.name, status = status, message = message ~= nil and tostring(message) or "" }
  end
  return out
end

--- Show text in a scrollable focused window.
---   content: a string (wrapped to fit; lines starting with # are headings)
---            or a list of lines
---   opts: { title, width = 100, height (fit, at most the screen) }
--- Returns a handle: handle:set(content), handle:close().
--- Keys: up/down/the wheel scroll, pageup/pagedown/space page, home/end,
--- esc or q close.
function bone.ui.pager(content, opts)
  opts = opts or {}
  local top, rows, total = 0, 1, 0
  local id

  local function body_lines(width)
    if type(content) ~= "string" then
      return content
    end
    local out, code = {}, false
    for line in (content .. "\n"):gmatch("(.-)\n") do
      local hl = "Normal"
      if line:match("^```") then
        code = not code
        hl = "Dim"
      elseif code then
        hl = "Dim"
      elseif line:match("^#+%s") then
        hl = "Accent"
      end
      if line == "" then
        out[#out + 1] = {}
      else
        for _, l in ipairs(bone.text.wrap({ { line, hl } }, width)) do
          out[#out + 1] = l
        end
      end
    end
    return out
  end

  local function render(ctx)
    local w = math.max(math.min(opts.width or 100, ctx.width - 2), 10)
    local inner = w - 4
    local lines = body_lines(inner)
    total = #lines
    local h = math.min(opts.height or (total + 3), ctx.height - 2)
    rows = math.max(h - 3, 1)
    top = math.max(0, math.min(top, total - rows))
    local body = {}
    for i = top + 1, math.min(total, top + rows) do
      body[#body + 1] = lines[i]
    end
    while #body < rows do
      body[#body + 1] = {}
    end
    local pos = total <= rows and "all" or string.format("%d-%d of %d", top + 1, math.min(total, top + rows), total)
    body[#body + 1] = { { pos .. " · ↑↓ scroll · esc close", "Dim" } }
    return bone.ui.box(body, { title = opts.title, width = w })
  end

  local function on_key(k)
    if k == "up" or k == "wheelup" or k == "k" then
      top = top - 1
    elseif k == "down" or k == "wheeldown" or k == "j" then
      top = top + 1
    elseif k == "pageup" then
      top = top - rows
    elseif k == "pagedown" or k == "space" then
      top = top + rows
    elseif k == "home" or k == "g" then
      top = 0
    elseif k == "end" or k == "G" then
      top = total
    elseif k == "esc" or k == "q" or k == "ctrl+c" then
      bone.ui.close(id)
    else
      return false
    end
    top = math.max(0, math.min(top, total - rows))
    return true
  end

  id = bone.ui.popup({ lines = render, on_key = on_key })
  local handle = {}
  function handle:set(new)
    content, top = new, 0
    bone.ui.update(id, {})
  end
  function handle:close()
    bone.ui.close(id)
  end
  return handle
end
