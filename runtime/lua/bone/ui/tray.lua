-- Session queue, sub-agents and shell jobs below the prompt.
-- ↓ on an empty prompt enters; ↑ from the top or esc returns. Tab/←/→
-- changes pages; ctrl+b folds. Enter/click opens an agent chat or live
-- shell; c cancels. Finished rows stay until the next message; ‹ main
-- returns from an agent. Queue: enter edits (enter saves, esc cancels),
-- s toggles steer/next or sends now when idle, shift+↑↓ reorders,
-- d drops and r resumes a paused queue. The rows and what they do are
-- bone.background, shared with other frontends' trays; this draws them for
-- the TUI and keeps the selection.
local M = {}

local CONTEXT, QUEUE_CONTEXT = "tray", "tray_queue"
local TERM_ROWS = 12 -- output rows of an open shell job

local bg = require("bone.background")
local ui = { page = "agents", sel = 1, open = nil, scroll = 0, folded = false }
local PAGES = bg.PAGES
-- What a click on each tray line does, from the last draw.
local hits = { tabs = {} }

local GLYPH, duration = bg.GLYPH, bg.duration

--- The rows of a page ("queue", "agents" or "shells") for the session on screen.
M.rows = bg.rows
local running = bg.running

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

local filled = bg.filled

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

M.back = bg.back
local queue = bg.queue

--- Put a queued message ({ id, text }) in the prompt to edit; enter saves
--- it in place, esc gives up.
function M.edit(r)
  leave()
  bg.edit(r)
end

--- esc while editing: leave the queued message as it was.
M.cancel_edit = bg.cancel_edit

-- s: steer ⇄ next while a turn runs; when idle, send it now.
local function steer()
  bg.steer(selected())
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
  bg.cancel(ui.page, selected())
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
  local back = bg.back_label()
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

local function row_line(r, width, cols, mark)
  local live = r.status == "running"
  local glyph = live and { "●", "Accent" } or GLYPH[r.status] or { "·", "Dim" }
  local time = r.time or duration((r.finished_at or bone.now()) - (r.started_at or bone.now()))
  if (r.tokens or 0) > 0 then
    time = bone.util.tokens(r.tokens) .. " tok · " .. time
  end
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
  local room = width - used - bone.text.width(time) - 2
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
        { " ● ", "Accent" },
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
    out[#out + 1] = row_line(r, ctx.width, cols, true)
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
    out[#out + 1] = row_line(rows[i], ctx.width, cols, active and r and rows[i].id == r.id)
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

-- Typing goes to the prompt, and so does the keyboard.
bone.on("prompt/changed", function()
  if focused() then
    leave()
  end
end)

---------------------------------------------------------------------------
-- Clicks on the tray (a sub-agent call in the chat: bone.background).

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
end)

return M
