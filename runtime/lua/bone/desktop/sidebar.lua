-- The desktop library's sessions sidebar (ctrl+o, /sessions): a drawer on
-- the left with your conversations as cards, recent ones first, a search
-- field, and a Processes page of every chat's background work. The data is
-- bone.chats; this draws it for the GUI. Up/down choose, enter opens, typing
-- searches, tab changes page, x stops a process, esc goes back to the
-- prompt, ctrl+o closes.
local M = {}

local chats = require("bone.chats")
local ID = "sessions"
M.width = 38

local panel
local st = { page = "chats", query = "", sel = 1, all = {}, stats = {}, items = {}, hits = {} }

local function refresh()
  if not (panel and panel:is_open()) then return end
  chats.fetch_list(function(all)
    if all then st.all = all end
    st.loading = false
    if panel then panel:update({}) end
  end)
  chats.fetch_stats(function(stats)
    if stats then st.stats = stats end
    if panel then panel:update({}) end
  end)
end

-- The rows of the page on screen, and what each line selects.
local function items()
  if st.page == "running" then
    return chats.jobs()
  end
  return chats.grouped(st.all, st.query, st.stats)
end

local function pad(text, width, hl)
  text = bone.text.truncate(text, width)
  return { text .. string.rep(" ", math.max(0, width - bone.text.width(text))), hl }
end

local function render(ctx)
  chats.note_running()
  local w = ctx.width
  local out, hits = {}, {}
  local function line(spans) out[#out + 1] = spans end
  local function blank() line({ pad("", w, "Panel") }) end
  local function text(t, hl) line({ pad("  " .. t, w, hl or "Panel") }) end
  blank()
  local jobs = #chats.jobs()
  line({
    { "  ", "Panel" },
    { "Chats", st.page == "chats" and "PanelAccent" or "PanelDim" },
    { "     ", "Panel" },
    { "Processes" .. (jobs > 0 and (" " .. jobs) or ""), st.page == "running" and "PanelAccent" or "PanelDim" },
    pad("", math.max(0, w - 18 - #tostring(jobs)), "Panel"),
  })
  st.tab_line = #out
  blank()
  local list = items()
  local rows = st.page == "running" and list or list.items
  st.items = rows
  st.sel = math.max(1, math.min(st.sel, #rows))
  if st.page == "chats" then
    text("⌕  " .. (st.query == "" and "Search chats" or st.query), st.query == "" and "PanelDim" or "Panel")
    blank()
  end
  if #rows == 0 then
    text(st.page == "running" and "Nothing running in the background"
      or st.loading and "Loading…" or (st.query == "" and "No chats yet" or "No matching chats"), "PanelDim")
  end
  local current = (bone.chat.session() or {}).session_id
  local group
  for i, r in ipairs(rows) do
    local on = i == st.sel
    local title_hl, meta_hl = on and "PanelSelected" or "Panel", on and "PanelSelectedDim" or "PanelDim"
    if st.page == "chats" then
      local g = list.groups[r.session_id]
      if g ~= group then
        if group then blank() end
        text(g == 1 and ("RECENT · " .. chats.recent_label()) or "HISTORY", "PanelDim")
        group = g
      end
      local live = chats.live[r.session_id]
      local mark = live and (ctx.spinner .. " ") or (r.session_id == current and "● " or "  ")
      local meta = st.stats[r.session_id]
      local detail = chats.timestamp(list.activities[r.session_id])
      if meta then detail = detail .. "  ·  " .. chats.count(meta.tokens) .. " tokens" end
      line({ { " ", "Panel" }, pad(" " .. mark .. (r.title or "[untitled]"), w - 2, title_hl), { " ", "Panel" } })
      hits[#out] = i
      line({ { " ", "Panel" }, pad("   " .. detail, w - 2, meta_hl), { " ", "Panel" } })
      hits[#out] = i
    else
      line({ { " ", "Panel" }, pad(" " .. ctx.spinner .. " " .. r.title, w - 2, title_hl), { " ", "Panel" } })
      hits[#out] = i
      local detail = bone.util.duration(os.time() - r.started) .. (r.detail ~= "" and ("  ·  " .. r.detail) or "")
      line({ { " ", "Panel" }, pad("   " .. detail, w - 2, meta_hl), { " ", "Panel" } })
      hits[#out] = i
    end
    blank()
  end
  st.hits = hits
  for _ = #out + 1, ctx.height do blank() end
  if bone.ui.refresh_in and (list.running or jobs > 0) then
    bone.ui.refresh_in((bone.ui.spinner and bone.ui.spinner.interval) or 120)
  end
  return out
end

local function choose(i)
  local r = st.items[i]
  if not r then return end
  st.sel = i
  bone.ui.panel.focus(nil)
  bone.api.open_session(r.session_id)
end

local function switch_page()
  st.page = st.page == "chats" and "running" or "chats"
  st.sel = 1
  if panel then panel:update({}) end
end

local function move(by)
  st.sel = math.max(1, math.min(#st.items, st.sel + by))
  if panel then panel:update({}) end
end

--- Open the sidebar, or close it when it has the keyboard.
function M.open()
  if panel and panel:is_open() then
    if bone.ui.panel.focused() == ID then panel:close() else panel:focus() end
    return
  end
  st.loading, st.sel = true, 1
  panel = bone.ui.panel.open({
    id = ID, dock = "left", full_height = true, separator = false, focus = true,
    size = tonumber(bone.state.load("gui-sessions-width").width) or M.width,
    render = render,
    keys = {
      esc = function() bone.ui.panel.focus(nil) end,
      ["ctrl+o"] = function() panel:close() end,
      tab = switch_page, ["shift+tab"] = switch_page,
      enter = function() choose(st.sel) end,
      up = function() move(-1) end, down = function() move(1) end,
      pageup = function() move(-5) end, pagedown = function() move(5) end,
    },
    on_key = function(key)
      if st.page == "running" then
        if key == "x" and st.items[st.sel] then chats.stop(st.items[st.sel]) return true end
        return false
      end
      local edited = bone.text.edit(st.query, key)
      if not edited then return false end
      st.query, st.sel = edited, 1
      panel:update({})
      return true
    end,
  })
  refresh()
  chats.fetch_shells(function() if panel then panel:update({}) end end)
  chats.fetch_active(function() if panel then panel:update({}) end end)
end

chats.watch(function(kind)
  if not (panel and panel:is_open()) then return end
  if kind == "turn" or kind == "created" or kind == "updated" or kind == "deleted" then
    refresh()
  else
    panel:update({})
  end
end)

bone.on("mouse", function(ev)
  if ev.panel ~= ID or ev.action ~= "down" or ev.button ~= "left" then return end
  if ev.panel_line == st.tab_line then
    local info = panel:info()
    if ((ev.x - info.col >= 9) and "running" or "chats") ~= st.page then switch_page() end
  elseif st.hits[ev.panel_line] then
    choose(st.hits[ev.panel_line])
  end
  return true
end)

return M
