-- The desktop library's help (/help, f1), document reader and /health, as
-- windows in the middle of the screen. Help: Commands, Keys and Docs
-- (click or tab), a search field (type to filter), each entry as a card
-- with what it does; enter puts a command in the prompt or opens its
-- documentation in the reader, which lays Markdown out as the chat does.
-- The content is bone.help's.
local M = {}

local help = require("bone.help")
local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local card = require("bone.desktop.card")
local popups = require("bone.nodes.popups")

local title, note, entry = popups.title, popups.note, popups.entry

--- Read Markdown `text` in a window titled `name`; esc closes it.
function M.read(text, name)
  local id
  local w, h = card.size()
  id = bone.ui.popup({
    width = w, height = h,
    node = function()
      local body = { direction = "column", size = md.text_size, padding = { 0, 10 } }
      nodes.append(body, md.nodes(text or ""))
      return popups.list("read:" .. id, { title(name or "Help"), body, note("wheel  scroll     esc  close") })
    end,
    on_key = function(k)
      if k == "esc" or k == "q" or k == "ctrl+c" then bone.ui.close(id) end
      return true
    end,
  })
  return id
end

--- A guide or section by name ("lua", "keys"), in the reader.
function M.topic(topic)
  local text, name = help.find(topic)
  if not text then
    return bone.notify("no help for " .. topic .. " (try /help lua or /help usage)", "error")
  end
  return M.read(text, name)
end

--- The help browser.
function M.open()
  local st = { tab = 1, query = "", sel = 1 }
  local id
  local list = {}
  local on_key
  local function choose(i)
    if help.choose(list[i], M.read) then bone.ui.close(id) end
  end
  local function node()
    list = help.list(st.tab, st.query)
    st.sel = math.max(1, math.min(st.sel, math.max(1, #list)))
    local items = { title("Help") }
    local tabs = {}
    for i, t in ipairs(help.TABS) do
      if i > 1 then tabs[#tabs + 1] = { text = "        ", hl = "Dim" } end
      tabs[#tabs + 1] = { text = t, hl = i == st.tab and "Normal" or "Dim", bold = i == st.tab }
    end
    items[#items + 1] = { spans = tabs, size = 14, padding = { 2, 10 }, margin = { 0, 0, 8, 0 },
      cursor = "pointer", on_click = function() on_key("tab") end }
    items[#items + 1] = {
      text = st.query == "" and "Type to search…" or st.query, hl = st.query == "" and "Dim" or "Normal",
      size = 13, bg = "InputBackground", radius = 10, padding = { 7, 12 }, margin = { 0, 0, 8, 0 },
    }
    if #list == 0 then items[#items + 1] = note("No matches.", { 4, 4 }) end
    local selected
    for i, it in ipairs(list) do
      local on = i == st.sel
      local lines = {
        { text = it.label, hl = it.command and "Accent" or "Normal", bold = on, size = 14 },
        { text = st.tab == 3 and it.meta or it.desc, hl = "Dim", size = 12, margin = { 3, 0, 0, 0 } },
      }
      if on and st.tab ~= 3 and it.meta ~= "" then
        lines[#lines + 1] = { text = it.meta, hl = "Dim", size = 11, margin = { 3, 0, 0, 0 } }
      end
      items[#items + 1] = entry(on, lines, function()
        if st.sel == i then choose(i) else st.sel = i end
        bone.ui.update(id, {})
      end)
      if on then selected = #items end
    end
    items[#items + 1] = note("tab  section     ↑↓  move     enter  open     esc  close")
    return popups.list("help:" .. id, items, selected)
  end
  function on_key(k)
    if k == "esc" or k == "ctrl+c" then
      bone.ui.close(id)
    elseif k == "tab" or k == "shift+tab" or k == "right" or k == "left" then
      local by = (k == "tab" or k == "right") and 1 or -1
      st.tab, st.query, st.sel = (st.tab - 1 + by) % #help.TABS + 1, "", 1
    elseif k == "up" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" then
      st.sel = math.min(math.max(1, #list), st.sel + 1)
    elseif k == "pageup" or k == "pagedown" then
      st.sel = math.max(1, math.min(math.max(1, #list), st.sel + (k == "pageup" and -8 or 8)))
    elseif k == "enter" then
      choose(st.sel)
    elseif bone.text.edit(st.query, k) then
      st.query, st.sel = bone.text.edit(st.query, k), 1
    else
      return false
    end
    bone.ui.update(id, {})
    return true
  end
  local w, h = card.size()
  id = bone.ui.popup({ width = w, height = h, node = node, on_key = on_key })
  return id
end

-- /health: this window's checks, then the core's.
local MARK = { ok = { "•", "ToolDone" }, warn = { "!", "WarningMsg" }, error = { "×", "ErrorMsg" } }
local function checks(name, items, out)
  out[#out + 1] = { text = name, hl = "Normal", bold = true, size = 15, padding = { 2, 10 }, margin = { 8, 0, 4, 0 } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = {
      direction = "column", bg = "Panel", radius = 10, padding = { 7, 12 }, margin = { 1, 0, 1, 0 },
      { spans = { { text = m[1] .. "   ", hl = m[2], bold = true }, { text = it.name, hl = "Normal", bold = true } }, size = 14 },
      { text = tostring(it.message), hl = "Dim", size = 12, margin = { 2, 0, 0, 0 }, padding = { 0, 0, 0, 22 } },
    }
  end
end

function M.health()
  local mine = bone.api.health()
  for _, it in ipairs(bone._run_health()) do mine[#mine + 1] = it end
  local core
  local id
  local w, h = card.size()
  id = bone.ui.popup({
    width = w, height = h,
    node = function()
      local items = { title("Health") }
      checks("This window", mine, items)
      if core then
        checks("Core", core, items)
      else
        items[#items + 1] = note("Core: checking…", { 8, 0 })
      end
      items[#items + 1] = note("esc  close")
      return popups.list("health:" .. id, items)
    end,
    on_key = function(k)
      if k == "esc" or k == "q" or k == "ctrl+c" then bone.ui.close(id) end
      return true
    end,
  })
  bone.request("health/check", {}, function(res, err)
    core = err and { { name = "core", status = "error", message = err } } or res or {}
    bone.ui.update(id, {})
  end)
  return id
end

return M
