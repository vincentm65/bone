-- The desktop library's tray under the composer: the chat's queued
-- messages, sub-agents and background shells, one quiet row each with
-- what they do and for how long. The rows and actions are
-- bone.background; this draws them for the GUI. Down on an empty prompt
-- enters it (tab changes page, enter opens, c cancels, esc or up from the
-- top leaves); a click opens a row.
local M = {}

local bg = require("bone.background")
local CONTEXT = "desktop_tray"
local ui = { page = "agents", sel = 1 }
local hits = {}

local function focused()
  return bone.keymap.current() == CONTEXT
end

local function rows_of()
  return { queue = bg.rows("queue"), agents = bg.rows("agents"), shells = bg.rows("shells") }
end

local function leave()
  if focused() then bone.keymap.focus(nil) end
end

--- Give the tray the keyboard; returns whether it had anything.
function M.enter()
  local pages = bg.filled()
  if #pages == 0 then return false end
  if pages[1] == "queue" or #bg.rows(ui.page) == 0 then ui.page = pages[1] end
  ui.sel = 1
  bone.keymap.focus(CONTEXT)
  return true
end

local function selected()
  local rows = bg.rows(ui.page)
  ui.sel = math.max(1, math.min(ui.sel, #rows))
  return rows[ui.sel]
end

local function open(r)
  r = r or selected()
  if not r then return end
  leave()
  if ui.page == "queue" then
    bg.edit(r)
  elseif ui.page == "shells" then
    -- A shell's output opens in a terminal-style pager.
    local screen = bone.processes.screen(r.id, { width = 100, height = 40 })
    bone.ui.pager(screen.lines, { title = r.title })
  else
    bone.api.open_session(r.id)
  end
end

local function switch(by)
  local pages = bg.filled()
  local at = 0
  for i, p in ipairs(pages) do at = p == ui.page and i or at end
  ui.page = pages[(at + by - 1) % math.max(#pages, 1) + 1] or ui.page
  ui.sel = 1
end

local function row(r, width, active)
  local live = r.status == "running"
  local glyph = live and { "●", "PanelRunning" } or bg.GLYPH[r.status] or { "·", "Dim" }
  local time = r.time or bg.duration((r.finished_at or bone.now()) - (r.started_at or bone.now()))
  local base = active and "PanelSelected" or "Normal"
  local line = { { active and "  › " or "    ", base }, { glyph[1] .. "  ", glyph[2] } }
  if r.name then line[#line + 1] = { r.name .. "   ", live and "Accent" or "Dim" } end
  line[#line + 1] = { bone.text.truncate(r.title or "", math.floor(width * 0.4)) .. "   ", live and base or "Dim" }
  if r.detail and r.detail ~= "" then
    line[#line + 1] = { bone.text.truncate(r.detail, math.max(0, math.floor(width * 0.35))), "Dim" }
  end
  line[#line + 1] = { fill = " ", hl = base }
  line[#line + 1] = { time .. "  ", "Dim" }
  return line
end

local function render(ctx)
  hits = {}
  local all = rows_of()
  if #all.queue + #all.agents + #all.shells == 0 then
    leave()
    return nil
  end
  local active = focused()
  if #all[ui.page] == 0 then ui.page = bg.filled()[1] or ui.page end
  local head = { { "  ", "Normal" } }
  local back = bg.back_label()
  if back then
    head[#head + 1] = { back .. "     ", "Accent" }
    hits.back = true
  end
  for _, pg in ipairs(bg.PAGES) do
    local rows = all[pg[1]]
    if pg[1] ~= "queue" or #rows > 0 then
      local n = bg.running(rows)
      head[#head + 1] = { pg[2] .. " " .. (n > 0 and n or #rows), ui.page == pg[1] and "StatusLine" or "Dim" }
      head[#head + 1] = { "     ", "Normal" }
    end
  end
  head[#head + 1] = { fill = " ", hl = "Normal" }
  head[#head + 1] = { active and "enter open · c cancel · esc  " or "↓ to manage  ", "Dim" }
  local out = { head }
  local rows = all[ui.page]
  local room = math.max(ctx.height - 1, 1)
  local first = math.max(1, math.min(ui.sel - room + 1, #rows - room + 1))
  for i = first, math.min(#rows, first + room - 1) do
    out[#out + 1] = row(rows[i], ctx.width, active and i == ui.sel)
    hits[#out] = i
  end
  return out
end

function M.setup()
  bone.ui.regions.tray = { size = "auto", max = 7, render = render }
  bone.keymap.context(CONTEXT, { fallback = "main" })
  local keys = {
    up = function() if ui.sel <= 1 then leave() else ui.sel = ui.sel - 1 end end,
    down = function() ui.sel = ui.sel + 1 end,
    tab = function() switch(1) end,
    ["shift+tab"] = function() switch(-1) end,
    enter = function() open() end,
    c = function() bg.cancel(ui.page, selected()) end,
    s = function() if ui.page == "queue" then bg.steer(selected()) end end,
    d = function() if ui.page == "queue" then bg.cancel("queue", selected()) end end,
    esc = leave,
  }
  for key, fn in pairs(keys) do
    bone.keymap.set(key, fn, { context = CONTEXT })
  end
  -- From the prompt: down enters, esc gives up an edit or goes back from a
  -- sub-agent, up on an empty prompt edits the last queued message.
  local actions = bone.ui.actions
  local menu_down, menu_dismiss, menu_up = actions.down, actions.dismiss, actions.up
  actions.down = function()
    if menu_down and menu_down() then return true end
    return bone.prompt.empty() and not bone.prompt.info().history and M.enter()
  end
  actions.dismiss = function()
    local done = menu_dismiss and menu_dismiss()
    if done then return done end
    return bg.cancel_edit() or (bone.prompt.empty() and bg.back())
  end
  actions.up = function()
    if menu_up and menu_up() then return true end
    local s = bone.chat.session()
    if not bone.prompt.empty() or not s or not s.session_id then return false end
    local queued = bone.chat.items({ kind = "queued" })
    if #queued == 0 then return false end
    bg.edit(queued[#queued])
    return true
  end
  bone.on("prompt/changed", function() if focused() then leave() end end)
  bone.on("mouse", function(ev)
    if ev.region ~= "tray" or ev.button ~= "left" or ev.action ~= "down" then return end
    local i = hits[ev.row or 0]
    if i then
      ui.sel = i
      open(bg.rows(ui.page)[i])
    elseif ev.row == 1 and hits.back then
      bg.back()
    end
    return true
  end)
end

return M
