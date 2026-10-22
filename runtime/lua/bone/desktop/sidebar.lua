-- The desktop library's sessions sidebar (ctrl+o, /sessions): a drawer on
-- the left, as the Android app has it. A heading with the Processes page
-- beside it, New chat, a search field, then your conversations as cards
-- in date groups (Today, Yesterday, This week, Earlier), the one selected
-- on a rounded highlight; the Processes page lists every chat's
-- background work. Drawn as GUI blocks; the data is bone.chats. Up/down
-- choose, enter opens, typing searches, tab changes page, x stops a
-- process, esc goes back to the prompt, ctrl+o closes; a click opens.
local M = {}

local chats = require("bone.chats")
local ID = "sessions"
M.width = 40

local panel
local st = { page = "chats", query = "", sel = 1, all = {}, stats = {}, items = {} }

local function redraw()
  if panel and panel:is_open() then panel:update({}) end
end

local function refresh()
  if not (panel and panel:is_open()) then return end
  chats.fetch_list(function(all)
    if all then st.all = all end
    st.loading = false
    redraw()
  end)
  chats.fetch_stats(function(stats)
    if stats then st.stats = stats end
    redraw()
  end)
end

-- "Today", "Yesterday", "This week" or "Earlier" for a time.
local function date_group(at)
  local day = os.date("*t", at)
  local today = os.date("*t")
  local midnight = os.time({ year = today.year, month = today.month, day = today.day, hour = 0 })
  if at >= midnight then return "Today" end
  if at >= midnight - 86400 then return "Yesterday" end
  if at >= midnight - 6 * 86400 then return "This week" end
  return "Earlier", day
end

local function folder(cwd)
  local name = (cwd or ""):gsub("/+$", ""):match("([^/]+)$")
  return name or "~"
end

local function label(text, opts)
  opts = opts or {}
  return {
    paragraphs = { { spans = { { text, opts.hl or "Dim" } }, size = opts.size or 12 } },
    pad = opts.pad or { 4, 14 }, margin = opts.margin or { 0, 0 },
  }
end

local function card(title, meta, on, live, width)
  local chars = math.max(10, math.floor(width / 8))
  local head = {}
  if live then head[#head + 1] = { text = "•  ", hl = "ToolRunning" } end
  head[#head + 1] = { text = bone.text.truncate(title, chars), hl = "Normal", bold = on }
  return {
    paragraphs = {
      { spans = head, size = 14 },
      { spans = { { bone.text.truncate(meta, chars + 6), "Dim" } }, size = 12, gap = 2 },
    },
    bg = on and "Selection" or nil, radius = 12, pad = { 8, 12 }, margin = { 1, 1 }, fill = true,
  }
end

local function blocks(ctx)
  chats.note_running()
  local out = {}
  local function add(id, block, selected) out[#out + 1] = { id = id, block = block, selected = selected } end
  local jobs = chats.jobs()
  add("tabs", {
    paragraphs = { {
      spans = {
        { text = "Chats", hl = st.page == "chats" and "Normal" or "Dim", bold = st.page == "chats" },
        { text = "      ", hl = "Dim" },
        { text = "Processes" .. (#jobs > 0 and (" " .. #jobs) or ""), hl = st.page == "running" and "Normal" or "Dim",
          bold = st.page == "running" },
      },
      size = 17,
    } },
    pad = { 14, 14 }, margin = { 0, 4 },
  })
  if st.page == "chats" then
    add("new", {
      paragraphs = { { spans = { { "+   New chat", "Normal" } }, size = 14 } },
      border = "PromptBorder", radius = 12, pad = { 8, 12 }, margin = { 2, 6 }, fill = true,
    })
    add("search", {
      paragraphs = { { spans = { { st.query == "" and "Search chats…" or st.query, st.query == "" and "Dim" or "Normal" } }, size = 13 } },
      bg = "InputBackground", radius = 10, pad = { 7, 12 }, margin = { 2, 10 }, fill = true,
    })
  end
  local rows
  if st.page == "running" then
    rows = jobs
    if #rows == 0 then add("none", label("Nothing running in the background", { size = 13 })) end
    for i, j in ipairs(rows) do
      local meta = bone.util.duration(os.time() - j.started) .. (j.detail ~= "" and ("  ·  " .. j.detail) or "")
      add(i, card(j.title, meta, i == st.sel, true, ctx.width), i == st.sel)
    end
  else
    local g = chats.grouped(st.all, st.query, st.stats)
    rows = g.items
    if #rows == 0 then
      add("none", label(st.loading and "Loading…" or (st.query == "" and "No chats yet" or "No matching chats"), { size = 13 }))
    end
    -- Newest first, in date groups.
    table.sort(rows, function(a, b)
      local aa, ba = g.activities[a.session_id], g.activities[b.session_id]
      if aa ~= ba then return aa > ba end
      return a.session_id < b.session_id
    end)
    local current = (bone.chat.session() or {}).session_id
    local group
    for i, s in ipairs(rows) do
      local at = g.activities[s.session_id]
      local name = date_group(at)
      if name ~= group then
        add("group:" .. name, label(name, { margin = { group and 10 or 2, 2 } }))
        group = name
      end
      local meta = { folder(s.cwd), chats.timestamp(at) }
      local stats = st.stats[s.session_id]
      if stats and (stats.tokens or 0) > 0 then meta[#meta + 1] = chats.count(stats.tokens) .. " tokens" end
      local on = i == st.sel or (st.sel == 0 and s.session_id == current)
      add(i, card(s.title or "Untitled", table.concat(meta, "  ·  "), on, chats.live[s.session_id], ctx.width), i == st.sel)
    end
  end
  st.items = rows
  st.sel = math.max(1, math.min(st.sel, #rows))
  if bone.ui.refresh_in and #jobs > 0 then
    bone.ui.refresh_in(1000)
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
  redraw()
end

local function move(by)
  st.sel = math.max(1, math.min(#st.items, st.sel + by))
  redraw()
end

local function on_block(id)
  if id == "tabs" then
    switch_page()
  elseif id == "new" then
    bone.ui.panel.focus(nil)
    bone.action("new_session")
  elseif type(id) == "number" then
    if st.page == "running" then
      st.sel = id
      redraw()
    else
      choose(id)
    end
  end
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
    size = M.width,
    blocks = blocks,
    on_block = on_block,
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
      redraw()
      return true
    end,
  })
  refresh()
  chats.fetch_shells(redraw)
  chats.fetch_active(redraw)
end

chats.watch(function(kind)
  if not (panel and panel:is_open()) then return end
  if kind == "turn" or kind == "created" or kind == "updated" or kind == "deleted" then
    refresh()
  else
    redraw()
  end
end)

return M
