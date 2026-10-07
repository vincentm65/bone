-- Session queue, sub-agents and shell jobs below the prompt.
-- ↓ on an empty prompt enters; ↑ from the top or esc returns. Tab/←/→
-- changes pages; ctrl+b folds. Enter/click opens an agent chat or live
-- shell; c cancels. Finished rows stay until the next message; ‹ main
-- returns from an agent. Queue: enter edits (enter saves, esc cancels),
-- s toggles steer/next or sends now when idle, shift+↑↓ reorders,
-- d drops and r resumes a paused queue.
local M = {}

local CONTEXT, QUEUE_CONTEXT = "tray", "tray_queue"
local TERM_ROWS = 12 -- output rows of an open shell job

-- Sub-agents seen this run: session id -> agent; tool call id -> session id.
local agents, by_call = {}, {}
-- Finished rows the user has moved past (sent a message since).
local cleared = {}
local ui = { page = "agents", sel = 1, open = nil, scroll = 0, folded = false }
local PAGES = { { "queue", "Queue" }, { "agents", "Agents" }, { "shells", "Shells" } }
-- The queued message being edited in the prompt: { session_id, id }.
local editing
-- What a click on each tray line does, from the last draw.
local hits = { tabs = {} }

local GLYPH = {
  done = { "✓", "ShellProgram" },
  failed = { "✗", "ToolError" },
  cancelled = { "⊘", "Dim" },
  timeout = { "⏱", "ToolError" },
  steer = { "↳", "Accent" },
  next = { "◦", "Dim" },
}

local function duration(ms)
  local s = math.max(0, math.floor((ms or 0) / 1000))
  if s < 60 then
    return s .. "s"
  elseif s < 3600 then
    return string.format("%dm%02ds", math.floor(s / 60), s % 60)
  end
  return string.format("%dh%02dm", math.floor(s / 3600), math.floor(s / 60) % 60)
end

local function last_line(text)
  local last
  for line in (text or ""):gmatch("[^\r\n]+") do
    if line:match("%S") then
      last = line
    end
  end
  return last and (last:gsub("^%s+", ""):gsub("%s+", " ")) or nil
end

-- "read_file src/main.rs", "shell cargo test", …; paths under `cwd` are
-- shown relative to it.
local function call_summary(call, cwd)
  local a = call.arguments
  if type(a) == "string" then
    local ok, v = pcall(bone.json.decode, a)
    a = ok and v or nil
  end
  a = type(a) == "table" and a or {}
  for _, k in ipairs({ "path", "command", "pattern", "query", "url", "task", "prompt" }) do
    if type(a[k]) == "string" and a[k] ~= "" then
      local v = a[k]:gsub("%s+", " ")
      local prefix = (cwd or ""):gsub("/$", "") .. "/"
      if #prefix > 1 and v:sub(1, #prefix) == prefix then
        v = v:sub(#prefix + 1)
      end
      return call.name .. " " .. v
    end
  end
  return call.name
end

---------------------------------------------------------------------------
-- Rows

-- The top-level session of the one on screen: the tray stays the same
-- while you look inside its sub-agents.
local function root()
  local s = bone.chat.session()
  if not s then
    return nil
  end
  local id = s.owner and s.owner.session_id or s.session_id
  while agents[id] do
    id = agents[id].parent
  end
  return id
end

local function by_start(a, b)
  return (a.started_at or 0) < (b.started_at or 0)
end

local function agent_rows(top)
  local out = {}
  for id, a in pairs(agents) do
    local r = a.parent
    while agents[r] do
      r = agents[r].parent
    end
    if r == top and not cleared["agent:" .. id] then
      out[#out + 1] = a
    end
  end
  table.sort(out, by_start)
  return out
end

local PROCESS_STATUS = { cancelled = "cancelled", timed_out = "timeout", failed = "failed" }

local function shell_rows()
  local out = {}
  for _, p in ipairs(bone.processes.list()) do
    -- Foreground commands show in the chat as tool calls already.
    if p.terminal and not cleared["shell:" .. p.id] then
      local status = "running"
      if not p.running then
        status = PROCESS_STATUS[p.state] or ((p.code or 0) == 0 and "done" or "failed")
      end
      local detail = p.tail or last_line(p.output)
      if status ~= "running" and status ~= "done" then
        detail = p.error or (p.code and ("exit " .. p.code)) or (p.signal and ("signal " .. p.signal)) or detail
      elseif status == "done" then
        detail = "exit 0"
      end
      out[#out + 1] = {
        key = "shell:" .. p.id,
        id = p.id,
        title = "$ " .. (p.command or p.id):gsub("%s+", " "),
        detail = detail,
        status = status,
        started_at = p.started_at_ms,
        finished_at = p.finished_at_ms,
      }
    end
  end
  table.sort(out, by_start)
  return out
end

local function queue_rows()
  local s = bone.chat.session()
  local rows = bone.chat.items({ kind = "queued" })
  for _, q in ipairs(rows) do
    q.key = s.session_id .. ":" .. q.id
    q.status, q.time, q.title = q.mode, "", q.text:gsub("%s+", " ")
    q.detail = s.queue_paused and "paused · r resume" or (q.mode == "steer" and "joins this turn" or "next turn")
  end
  return rows
end

--- The rows of a page ("queue", "agents" or "shells") for the session on screen.
function M.rows(page)
  if page == "queue" then
    return queue_rows()
  elseif page == "shells" then
    return shell_rows()
  end
  local top = root()
  return top and agent_rows(top) or {}
end

local function running(rows)
  local n = 0
  for _, r in ipairs(rows) do
    if r.status == "running" then
      n = n + 1
    end
  end
  return n
end

---------------------------------------------------------------------------
-- Focus and actions

local function focused()
  return bone.keymap.current() == CONTEXT or bone.keymap.current() == QUEUE_CONTEXT
end

local function selected(rows)
  rows = rows or M.rows(ui.page)
  ui.sel = math.max(1, math.min(ui.sel, #rows))
  if ui.page == "queue" and ui.queue_id then
    for i, r in ipairs(rows) do
      if r.key == ui.queue_id then
        ui.sel = i
        return r
      end
    end
    return nil
  end
  return rows[ui.sel]
end

local function leave()
  ui.open = nil
  if focused() then
    bone.keymap.focus(nil)
  end
end

local function focus()
  ui.folded = false
  bone.keymap.focus(ui.page == "queue" and QUEUE_CONTEXT or CONTEXT)
end

-- The pages with rows, in tab order.
local function filled()
  local out = {}
  for _, pg in ipairs(PAGES) do
    if #M.rows(pg[1]) > 0 then
      out[#out + 1] = pg[1]
    end
  end
  return out
end

--- Give the tray the keyboard, on its first row (the queue when it has
--- any). Returns whether there was anything in it.
function M.enter()
  local pages = filled()
  if #pages == 0 then
    return false
  end
  if pages[1] == "queue" or #M.rows(ui.page) == 0 then
    ui.page = pages[1]
  end
  ui.sel, ui.queue_id = 1, nil
  focus()
  return true
end

-- To `page`, or `by` tabs along (wrapping round the pages with rows).
local function switch(page, by)
  if not page then
    local pages = filled()
    local at = 0
    for i, p in ipairs(pages) do
      at = p == ui.page and i or at
    end
    page = pages[(at + (by or 1) - 1) % math.max(#pages, 1) + 1] or ui.page
  end
  ui.page = page
  ui.sel, ui.open, ui.scroll, ui.queue_id = 1, nil, 0, nil
  if focused() then focus() end
end

local function move(by)
  selected()
  ui.queue_id = nil
  local n = #M.rows(ui.page)
  if ui.sel + by < 1 then
    return leave()
  end
  ui.sel = math.max(1, math.min(ui.sel + by, n))
end

--- Back to the session that started the one on screen. Returns whether
--- there was one.
function M.back()
  local s = bone.chat.session()
  if s and s.owner and s.owner.session_id then
    bone.api.open_session(s.owner.session_id)
    return true
  end
  return false
end

-- queue/<method> for the session on screen; errors are shown.
local function queue(method, params, after)
  local s = bone.chat.session()
  params.session_id = params.session_id or (s and s.session_id)
  if not params.session_id then
    return
  end
  bone.request("queue/" .. method, params, function(_, err)
    if err then
      bone.notify(tostring(err), "error")
    elseif after then
      after(params.session_id)
    end
  end)
end

--- Put a queued message ({ id, text }) in the prompt to edit; enter saves
--- it in place, esc gives up.
function M.edit(r)
  local s = bone.chat.session()
  leave()
  bone.prompt.set(r.text)
  editing = { session_id = s.session_id, id = r.id }
  bone.notify("editing a queued message · enter saves · esc cancels")
end

--- esc while editing: leave the queued message as it was.
function M.cancel_edit()
  if not editing then
    return false
  end
  editing = nil
  bone.prompt.set("")
  return true
end

-- s: steer ⇄ next while a turn runs; when idle, send it now.
local function steer()
  local r, s = selected(), bone.chat.session()
  if not (r and s) then
    return
  elseif s.running then
    return queue("update", { id = r.id, mode = r.mode == "steer" and "next" or "steer" })
  end
  queue("move", { id = r.id, to = 0 }, function(id) queue("resume", { session_id = id }) end)
end

local function reorder(by)
  local r = selected()
  local to = ui.sel + by
  if r and to >= 1 and to <= #M.rows("queue") then
    ui.queue_id = r.key
    queue("move", { id = r.id, to = to - 1 })
  end
end

local function open()
  local r = selected()
  if not r then
    return
  end
  if ui.page == "queue" then
    M.edit(r)
  elseif ui.page == "shells" then
    ui.open, ui.scroll = r.id, 0
  else
    leave()
    bone.api.open_session(r.id)
  end
end

local function cancel()
  local r = selected()
  if r and ui.page == "queue" then
    return queue("remove", { id = r.id })
  elseif not (r and r.status == "running") then
    return
  end
  if ui.page == "shells" then
    bone.processes.cancel(r.id)
  else
    bone.request("turn/cancel", { session_id = r.id })
  end
end

local function scroll(by)
  ui.scroll = math.max(ui.scroll + by, 0)
end

--- ctrl+b: fold the tray to one line, or unfold it.
function M.toggle()
  if focused() then
    leave()
    ui.folded = true
  else
    ui.folded = not ui.folded
  end
end

---------------------------------------------------------------------------
-- Drawing

local function pad(text, w)
  text = bone.text.truncate(text, w)
  return text .. string.rep(" ", w - bone.text.width(text))
end

-- "‹ main", or "‹ <agent>" inside a sub-agent's sub-agent; nil at the top.
local function back_label()
  local s = bone.chat.session()
  local parent = s and s.owner and s.owner.session_id
  if not parent then
    return nil
  end
  return "‹ " .. (agents[parent] and agents[parent].name or "main")
end

local function header(width, active, rows_of)
  local items, col = {}, 1
  hits.tabs = {}
  local function add(text, hl)
    items[#items + 1] = { text, hl }
    col = col + bone.text.width(text)
  end
  local function tab(from, fn)
    hits.tabs[#hits.tabs + 1] = { from, col - 1, fn }
  end
  local back = back_label()
  if back then
    local from = col
    add(" " .. back .. " ", "Accent")
    tab(from, M.back)
    add("│", "WinSeparator")
  end
  for _, pg in ipairs(PAGES) do
    local rows = rows_of[pg[1]]
    if pg[1] ~= "queue" or #rows > 0 then
      local n = running(rows)
      local on = active and ui.page == pg[1]
      local from = col
      add(" " .. pg[2] .. " ", on and "StatusLine" or "Dim")
      add(tostring(n > 0 and n or #rows) .. " ", n > 0 and (on and "Accent" or "ToolArgs") or "Dim")
      tab(from, function()
        switch(pg[1])
        focus()
      end)
    end
  end
  local hint = "↓ or ctrl+b"
  if ui.open then
    local r = selected()
    hint = (r and r.status == "running" and "c cancel · " or "") .. "↑↓ scroll · esc close"
  elseif active and ui.page == "queue" then
    hint = "enter edit · s steer · ⇧↑↓ move · d drop · esc"
  elseif active then
    hint = "↑↓ move · enter open · c cancel · esc"
  end
  items[#items + 1] = { fill = " ", hl = "Normal" }
  items[#items + 1] = { hint .. " ", "Dim" }
  return items
end

-- Column widths for a page's rows: as wide as the widest needs, capped so
-- the details line up.
local function columns(rows, width)
  local name_w, title_w = 0, 0
  for _, r in ipairs(rows) do
    name_w = math.max(name_w, r.name and bone.text.width(r.name) or 0)
    title_w = math.max(title_w, bone.text.width(r.title or ""))
  end
  name_w = math.min(name_w, 12)
  title_w = math.min(title_w, math.floor(width * (name_w > 0 and 0.3 or 0.45)))
  return name_w, title_w
end

local function row_line(r, width, cols, mark, spinner)
  local live = r.status == "running"
  local glyph = live and { spinner ~= "" and spinner or "◐", "Accent" } or GLYPH[r.status] or { "·", "Dim" }
  local time = r.time or duration((r.finished_at or bone.now()) - (r.started_at or bone.now()))
  local line = { { mark and " › " or "   ", "Accent" }, { glyph[1] .. " ", glyph[2] } }
  local used = 5
  if cols[1] > 0 then
    line[#line + 1] = { pad(r.name or "", cols[1]) .. "  ", live and "Accent" or "Dim" }
    used = used + cols[1] + 2
  end
  local title_hl = not live and "Dim" or r.name and "Normal" or "ShellProgram"
  line[#line + 1] = { pad(r.title or "", cols[2]) .. "  ", title_hl }
  used = used + cols[2] + 2
  local detail = r.detail or (live and "starting" or "")
  local room = width - used - #time - 2
  if room >= 6 and detail ~= "" then
    line[#line + 1] = { bone.text.truncate(detail, room), "Dim" }
  end
  line[#line + 1] = { fill = " ", hl = "Normal" }
  line[#line + 1] = { time .. " ", live and "ToolArgs" or "Dim" }
  return line
end

local function render(ctx)
  hits = { tabs = {} }
  local rows_of = { queue = M.rows("queue"), agents = M.rows("agents"), shells = M.rows("shells") }
  local all = #rows_of.queue + #rows_of.agents + #rows_of.shells
  local live = running(rows_of.agents) + running(rows_of.shells)
  local active = focused()
  if all == 0 then
    if active then
      leave()
    end
    return nil
  end
  local spinner = ctx.spinner or ""
  if ui.folded and not active then
    if live == 0 then
      return nil
    end
    local a, s = running(rows_of.agents), running(rows_of.shells)
    hits[1] = function()
      ui.folded = false
    end
    return {
      {
        { " " .. (spinner ~= "" and spinner or "◐") .. " ", "Accent" },
        { string.format("%d agent%s · %d shell%s", a, a == 1 and "" or "s", s, s == 1 and "" or "s"), "ToolArgs" },
        { fill = " ", hl = "Normal" },
        { "ctrl+b ", "Dim" },
      },
    }
  end

  if #rows_of[ui.page] == 0 and not (active and ui.page ~= "queue" or ui.open) then
    ui.page = filled()[1] or ui.page
    if active then focus() end
  end
  local rows = rows_of[ui.page]
  local r = selected(rows)
  local cols = { columns(rows, ctx.width) }
  local out = { header(ctx.width, active or ui.open, rows_of) }

  -- An open shell job: its row, then its terminal.
  if ui.open and not (r and r.id == ui.open) then
    ui.open = nil
  end
  if ui.open then
    out[#out + 1] = row_line(r, ctx.width, cols, true, spinner)
    hits[#out] = function()
      ui.open = nil
    end
    out[#out + 1] = { { fill = "─", hl = "WinSeparator" } }
    local height = math.max(ctx.height - 3, 1)
    local screen = bone.processes.screen(r.id, { width = ctx.width - 3, height = height, scroll = ui.scroll })
    ui.scroll = screen.scroll
    for i = 1, height do
      local l = screen.lines[i]
      local line = { "   " }
      for _, item in ipairs(type(l) == "table" and l or { l }) do
        line[#line + 1] = item
      end
      out[#out + 1] = line
    end
    return out
  end

  if #rows == 0 then
    out[#out + 1] = { { "   nothing here", "Dim" } }
    return out
  end
  -- Keep the selection in view when there are more rows than room.
  local room = math.max(ctx.height - 1, 1)
  local first = math.max(1, math.min(ui.sel - room + 1, #rows - room + 1))
  for i = first, math.min(#rows, first + room - 1) do
    out[#out + 1] = row_line(rows[i], ctx.width, cols, active and r and rows[i].id == r.id, spinner)
    hits[#out] = function()
      ui.sel, ui.queue_id = i, nil
      if ui.page == "shells" then
        focus()
      end
      open()
    end
  end
  return out
end

M.region = { size = "auto", max = TERM_ROWS + 3, render = render }
bone.ui.regions.tray = M.region

---------------------------------------------------------------------------
-- Unmapped text returns to the prompt; queue controls have their own context.
local keys = {
  up = { function() move(-1) end, function() scroll(1) end },
  down = { function() move(1) end, function() scroll(-1) end },
  pageup = { nil, function() scroll(TERM_ROWS - 1) end },
  pagedown = { nil, function() scroll(1 - TERM_ROWS) end },
  home = { function() ui.sel, ui.queue_id = 1, nil end, function() scroll(1e9) end },
  ["end"] = { function() ui.sel, ui.queue_id = 1e9, nil end, function() ui.scroll = 0 end },
  wheelup = { function() move(-1) end, function() scroll(3) end },
  wheeldown = { function() move(1) end, function() scroll(-3) end },
  tab = { switch, switch },
  ["shift+tab"] = { function() switch(nil, -1) end, switch },
  enter = { open },
  c = { cancel, cancel },
  s = { steer, context = QUEUE_CONTEXT },
  r = { function() queue("resume", {}) end, context = QUEUE_CONTEXT },
  ["shift+up"] = { function() reorder(-1) end, context = QUEUE_CONTEXT },
  ["shift+down"] = { function() reorder(1) end, context = QUEUE_CONTEXT },
  esc = { leave, function() ui.open = nil end },
}
keys.k, keys.j = keys.up, keys.down
keys.left, keys.right = keys["shift+tab"], keys.tab
keys.d = { cancel, context = QUEUE_CONTEXT }
keys.delete = keys.d
keys.q = keys.esc

bone.keymap.context(CONTEXT, { fallback = "main" })
bone.keymap.context(QUEUE_CONTEXT, { fallback = CONTEXT })
for key, actions in pairs(keys) do
  bone.keymap.set(key, function()
    local fn = actions[ui.open and 2 or 1]
    if fn then fn() end
  end, { context = actions.context or CONTEXT })
end
bone.keymap.set("ctrl+b", M.toggle)

-- Typing goes to the prompt, and so does the keyboard. Emptying the
-- prompt gives up an edit.
bone.on("prompt/changed", function()
  if focused() then
    leave()
  end
  if editing and bone.prompt.get() == "" then
    editing = nil
  end
end)

-- Enter while editing a queued message saves it in place instead of sending.
bone.on("submit", function(ev)
  local e, s = editing, bone.chat.session()
  editing = nil
  if not (e and s and s.session_id == e.session_id) then
    return
  end
  bone.request("queue/update", { session_id = e.session_id, id = e.id, text = ev.text }, function(_, err)
    if err and (bone.chat.session() or {}).session_id == e.session_id then
      local draft = bone.prompt.get()
      bone.prompt.set(draft:match("%S") and draft .. "\n" .. ev.text or ev.text)
      bone.notify("it was sent before the edit was saved; your text is back in the prompt", "error")
    end
  end)
  return false
end)

-- Sending a message clears the finished rows.
bone.on("submit", function()
  for _, pg in ipairs({ "agents", "shells" }) do
    for _, r in ipairs(M.rows(pg)) do
      if r.status ~= "running" then
        cleared[r.key] = true
      end
    end
  end
end)

---------------------------------------------------------------------------
-- Sub-agents, from the core's events

bone.on("session/created", function(info)
  local o = info.owner
  if not o then
    return
  end
  agents[info.session_id] = {
    key = "agent:" .. info.session_id,
    id = info.session_id,
    name = o.name or "agent",
    title = info.title or "sub-agent",
    parent = o.session_id,
    cwd = info.cwd,
    status = "running",
    started_at = bone.now(),
    stream = "",
  }
  if o.call_id then
    by_call[o.call_id] = info.session_id
  end
end)

bone.on("turn/started", function(ev)
  local a = agents[ev.session_id]
  if a and a.status ~= "running" then
    a.status, a.finished_at, a.started_at, a.detail = "running", nil, bone.now(), nil
    cleared[a.key] = nil
  end
end)

bone.on("message/delta", function(ev)
  local a = agents[ev.session_id]
  if not a then
    return
  end
  if ev.kind == "reasoning" then
    if a.stream == "" then
      a.detail = "thinking"
    end
    return
  end
  a.stream = (a.stream .. (ev.text or "")):sub(-400):gsub("^[\128-\191]+", "")
  a.detail = last_line(a.stream) or a.detail
end)

bone.on("tool/started", function(ev)
  local a = agents[ev.session_id]
  if a and ev.call then
    a.detail, a.stream = call_summary(ev.call, a.cwd), ""
  end
end)

bone.on("turn/finished", function(ev)
  local a = agents[ev.session_id]
  if not a then
    return
  end
  local o = ev.outcome or {}
  a.status = ({ completed = "done", cancelled = "cancelled" })[o.status] or "failed"
  a.finished_at = bone.now()
  a.detail = o.status == "completed" and "done" or o.message or a.status
end)

bone.on("session/deleted", function(ev)
  agents[ev.session_id] = nil
end)

---------------------------------------------------------------------------
-- Clicks: the tray, and a subagent call in the chat.

bone.on("mouse", function(ev)
  if ev.button ~= "left" or ev.action ~= "down" then
    return
  end
  if ev.region == "tray" then
    local hit = hits[ev.row or 0]
    if hit then
      hit()
    elseif ev.row == 1 then
      for _, t in ipairs(hits.tabs) do
        if (ev.col or 0) >= t[1] and ev.col <= t[2] then
          t[3]()
        end
      end
    end
    return true
  end
  if ev.index then
    local it = bone.chat.item(ev.index)
    local id = it and it.kind == "tool" and it.name == "subagent" and by_call[it.id]
    if id then
      bone.api.open_session(id)
      return true
    end
  end
end)

return M
