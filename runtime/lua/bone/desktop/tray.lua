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

--- The tray as a node (the node view): nil when nothing is running.
function M.node()
  local all = rows_of()
  if #all.queue + #all.agents + #all.shells == 0 then
    leave()
    return nil
  end
  if #all[ui.page] == 0 then ui.page = bg.filled()[1] or ui.page end
  local active = focused()
  local head = { direction = "row", gap = 18, padding = { 2, 0, 4, 0 }, align = "center" }
  local back = bg.back_label()
  if back then
    head[#head + 1] = { text = back, hl = "Accent", size = 13, cursor = "pointer", on_click = bg.back }
  end
  for _, pg in ipairs(bg.PAGES) do
    local rows = all[pg[1]]
    if pg[1] ~= "queue" or #rows > 0 then
      local n = bg.running(rows)
      local name = pg[1]
      head[#head + 1] = {
        text = pg[2] .. " " .. (n > 0 and n or #rows), size = 13, cursor = "pointer",
        hl = ui.page == name and "StatusLine" or "Dim", bold = ui.page == name,
        on_click = function() ui.page, ui.sel = name, 1 end,
      }
    end
  end
  head[#head + 1] = { grow = 1 }
  head[#head + 1] = { text = active and "enter open · c cancel · esc" or "↓ to manage", hl = "Dim", size = 12 }
  local out = { direction = "column", head }
  local rows = all[ui.page]
  ui.sel = math.max(1, math.min(ui.sel, #rows))
  for i = 1, math.min(#rows, 6) do
    local r = rows[i]
    local live = r.status == "running"
    local glyph = live and { "●", "PanelRunning" } or bg.GLYPH[r.status] or { "·", "Dim" }
    local time = r.time or bg.duration((r.finished_at or bone.now()) - (r.started_at or bone.now()))
    local on = active and i == ui.sel
    local line = {
      direction = "row", gap = 8, align = "center", padding = { 3, 8, 3, 8 }, radius = 8,
      bg = on and "Selection" or nil, cursor = "pointer", hover = { bg = "InputBackground" }, transition = 120,
      on_click = function() ui.sel = i; open(r) end,
      { text = glyph[1], hl = glyph[2], size = 13 },
    }
    if r.name then line[#line + 1] = { text = r.name, hl = live and "Accent" or "Dim", size = 13 } end
    line[#line + 1] = { text = r.title or "", hl = live and "Normal" or "Dim", size = 13, wrap = false, shrink = 1, max_width = "45%", overflow = "hidden" }
    if r.detail and r.detail ~= "" then
      line[#line + 1] = { text = r.detail, hl = "Dim", size = 12, wrap = false, shrink = 1, overflow = "hidden" }
    end
    line[#line + 1] = { grow = 1 }
    line[#line + 1] = { text = time, hl = "Dim", size = 12 }
    out[#out + 1] = line
  end
  return out
end

function M.setup()
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
end

return M
