-- The background work of the chat on screen, as data and actions for a
-- tray to draw: its queued messages, sub-agents and shell jobs. Every
-- frontend's tray uses this and draws it its own way
-- (runtime/lua/bone/ui/tray.lua for the TUI). It also edits a queued
-- message in the prompt (enter saves it in place), clears finished rows
-- when a message is sent, and opens a sub-agent from its call in the chat.
local M = {}

local agents = require("bone.agents")
-- Finished rows the user has moved past (sent a message since).
local cleared = {}
-- The queued message being edited in the prompt: { session_id, id }.
local editing

--- The pages, in tab order: { name, label }.
M.PAGES = { { "queue", "Queue" }, { "agents", "Agents" }, { "shells", "Shells" } }

--- A row's look by status: { glyph, group }.
M.GLYPH = {
  done = { "✓", "ShellProgram" },
  failed = { "✗", "ToolError" },
  cancelled = { "⊘", "Dim" },
  timeout = { "⏱", "ToolError" },
  steer = { "↳", "Accent" },
  next = { "◦", "Dim" },
}

--- 75000 ms as "1m15s".
function M.duration(ms)
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

--- The top-level session of the one on screen: the tray stays the same
--- while you look inside its sub-agents.
function M.root()
  local s = bone.chat.session()
  if not s then
    return nil
  end
  return agents.top(s.owner and s.owner.session_id or s.session_id)
end

local function by_start(a, b)
  return (a.started_at or 0) < (b.started_at or 0)
end

local function agent_rows(top)
  local out = {}
  for _, a in ipairs(agents.list(top)) do
    if not cleared[a.key] then
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

--- The rows of a page ("queue", "agents" or "shells") for the session on
--- screen: { key, id, title, detail, status, started_at, finished_at,
--- name?, tokens?, time? }.
function M.rows(page)
  if page == "queue" then
    return queue_rows()
  elseif page == "shells" then
    return shell_rows()
  end
  local top = M.root()
  return top and agent_rows(top) or {}
end

--- How many of `rows` are running.
function M.running(rows)
  local n = 0
  for _, r in ipairs(rows) do
    if r.status == "running" then
      n = n + 1
    end
  end
  return n
end

--- The pages with rows, in tab order.
function M.filled()
  local out = {}
  for _, pg in ipairs(M.PAGES) do
    if #M.rows(pg[1]) > 0 then
      out[#out + 1] = pg[1]
    end
  end
  return out
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

--- "‹ main", or "‹ <agent>" inside a sub-agent's sub-agent; nil at the top.
function M.back_label()
  local s = bone.chat.session()
  local parent = s and s.owner and s.owner.session_id
  if not parent then
    return nil
  end
  return "‹ " .. (agents.get(parent) and agents.get(parent).name or "main")
end

--- queue/<method> for the session on screen; errors are shown.
function M.queue(method, params, after)
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
  bone.prompt.set(r.text)
  bone.prompt.set_images(r.images or {})
  editing = { session_id = s.session_id, id = r.id, has_images = #(r.images or {}) > 0 }
  bone.notify("editing a queued message · enter saves · esc cancels")
end

--- esc while editing: leave the queued message as it was.
function M.cancel_edit()
  if not editing then
    return false
  end
  editing = nil
  bone.prompt.set_images({})
  bone.prompt.set("")
  return true
end

--- A queued message: steer ⇄ next while a turn runs; when idle, send it now.
function M.steer(r)
  local s = bone.chat.session()
  if not (r and s) then
    return
  elseif s.running then
    return M.queue("update", { id = r.id, mode = r.mode == "steer" and "next" or "steer" })
  end
  M.queue("move", { id = r.id, to = 0 }, function(id) M.queue("resume", { session_id = id }) end)
end

--- Cancel row `r` of `page`: drop a queued message, stop a running shell
--- or sub-agent.
function M.cancel(page, r)
  if r and page == "queue" then
    return M.queue("remove", { id = r.id })
  elseif not (r and r.status == "running") then
    return
  end
  if page == "shells" then
    bone.processes.cancel(r.id)
  else
    bone.request("turn/cancel", { session_id = r.id })
  end
end

-- Emptying the prompt gives up an edit.
bone.on("prompt/changed", function()
  if editing and bone.prompt.empty() then
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
  local params = { session_id = e.session_id, id = e.id, text = ev.text }
  if e.has_images or #(ev.images or {}) > 0 then params.images = ev.images end
  bone.request("queue/update", params, function(_, err)
    if err and (bone.chat.session() or {}).session_id == e.session_id then
      local draft = bone.prompt.get()
      bone.prompt.set(draft:match("%S") and draft .. "\n" .. ev.text or ev.text)
      local images = bone.prompt.images()
      for _, image in ipairs(ev.images or {}) do images[#images + 1] = image end
      bone.prompt.set_images(images)
      bone.notify("queue edit failed: " .. tostring(err) .. "; message restored to its draft", "error")
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

-- Sub-agents (bone.agents keeps them): a rerun shows again; the step label.
bone.on("turn/started", function(ev)
  local a = agents.get(ev.session_id)
  if a then
    cleared[a.key] = nil
  end
end)

bone.on("tool/started", function(ev)
  local a = agents.get(ev.session_id)
  if a and ev.call then
    a.detail = call_summary(ev.call, a.cwd)
  end
end)

-- A click on a sub-agent call in the chat opens its chat (its report's
-- lines stay for expanding and collapsing).
bone.on("mouse", function(ev)
  if ev.button ~= "left" or ev.action ~= "down" or not ev.index or (ev.line or 1) > 2 then
    return
  end
  local it = bone.chat.item(ev.index)
  local a = it and it.kind == "tool" and it.name == "subagent" and agents.for_call(it.id)
  if a then
    bone.api.open_session(a.id)
    return true
  end
end)

return M
