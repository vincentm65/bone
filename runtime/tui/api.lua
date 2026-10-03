-- The TUI Lua API. Thin wrappers over `bone._api`, which runs each operation
-- directly against the UI. Loaded before runtime/tui/defaults.lua and
-- ~/.bone/tui.lua.

local api = bone._api

bone.keymap = {}

--- Map a key ("ctrl+s", "alt+enter", "shift+tab", "pageup", "f5", "?") to an
--- action: a builtin name ("submit", "page_up", ...), a slash command
--- ("/sessions"), or a Lua function.
--- opts.context: "main" (default, the prompt), "popup" (while a Lua popup is
--- open, after the popup's own keys) or "picker" (the session list).
function bone.keymap.set(key, action, opts)
  api("keymap_set", key, action, opts or {})
end

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
--- fn receives { args = "..." }.
function bone.cmd.create(name, fn, opts)
  api("command_create", name, fn, opts or {})
end

function bone.cmd.del(name)
  api("command_del", name)
end

--- Options: bone.o.show_reasoning = false; print(bone.o.tool_preview_lines)
bone.o = setmetatable({}, {
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
  prompt_get = function()
    return api("prompt_get")
  end,
  prompt_set = function(text)
    api("prompt_set", text)
  end,
  --- The session on screen, or nil: { session_id, cwd, title, running }
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

--- How the screen is drawn, all in Lua. Nothing is defined by default (Rust
--- draws plain text); examples/plugins/style is a full look. Lines are lists of { "text", "Group" } items (or
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
  })
end

bone.ui = { views = watched(), tool_views = watched(), regions = {} }

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
    local row = { { c[4] .. pad, bhl } }
    local used = 0
    for _, sp in ipairs(bone.text.clip(spans(l), inner)) do
      row[#row + 1] = sp
      used = used + bone.text.width(sp[1])
    end
    row[#row + 1] = { string.rep(" ", inner - used) .. pad, "Normal" }
    row[#row + 1] = { c[4], bhl }
    out[#out + 1] = row
  end
  out[#out + 1] = { { c[6] .. string.rep(c[2], inner + 2 * #pad) .. c[5], bhl } }
  return out
end

bone.chat = {}

--- Items of the session on screen, as views receive them.
--- opts: { kind = "reasoning", last = 1 }
function bone.chat.items(opts)
  return api("chat_items", opts or {})
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

