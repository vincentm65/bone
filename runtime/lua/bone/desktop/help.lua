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
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local title, note, entry, hints = widgets.title, widgets.note, widgets.entry, widgets.hints

--- Read Markdown `text` in a window titled `name`; esc closes it.
function M.read(text, name)
  local id
  id = bone.ui.popup({
    node = function()
      local body = { direction = "column", size = md.text_size, padding = { 0, 10 } }
      nodes.append(body, md.nodes(text or ""))
      return widgets.list("read:" .. id, { title(name or "Help"), body },
        nil, { footer = hints("wheel scroll · esc close", { 10, 0 }) })
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
    items[#items + 1] = widgets.tabs(help.TABS, st.tab, function(i)
      st.tab, st.query, st.sel = i, "", 1
      bone.ui.update(id, {})
    end)
    items[#items + 1] = {
      text = st.query == "" and "Type to search…" or (st.query .. "▏"), hl = st.query == "" and "Dim" or "Normal",
      size = style.size.caption, bg = "InputBackground", radius = style.radius.lg, padding = { 7, 12 },
      margin = { 0, 0, 8, 0 },
    }
    if #list == 0 then items[#items + 1] = note("No matches.", { 4, 4 }) end
    local selected
    for i, it in ipairs(list) do
      local on = i == st.sel
      local lines = {
        { text = it.label, hl = it.command and "Accent" or "Normal", bold = on, size = style.size.body },
        { text = st.tab == 3 and it.meta or it.desc, hl = "Dim", size = style.size.small, margin = { 3, 0, 0, 0 } },
      }
      if on and st.tab ~= 3 and it.meta ~= "" then
        lines[#lines + 1] = { text = it.meta, hl = "Dim", size = style.size.micro, margin = { 3, 0, 0, 0 } }
      end
      items[#items + 1] = entry(on, lines, function()
        if st.sel == i then choose(i) else st.sel = i end
        bone.ui.update(id, {})
      end)
      if on then selected = #items end
    end
    return widgets.list("help:" .. id, items, selected,
      { footer = hints("tab section · ↑↓ move · enter open · esc close", { 10, 0 }) })
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
  id = bone.ui.popup({ node = node, on_key = on_key })
  return id
end

-- /health: this window's checks, then the core's.
local MARK = { ok = { "•", "ToolDone" }, warn = { "!", "WarningMsg" }, error = { "×", "ErrorMsg" } }
local function checks(name, items, out)
  out[#out + 1] = { text = name, hl = "Normal", bold = true, size = style.size.text, padding = { 2, 10 }, margin = { 8, 0, 4, 0 } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = {
      direction = "column", bg = "Panel", radius = style.radius.lg, padding = { 7, 12 }, margin = { 1, 0, 1, 0 },
      { spans = { { text = m[1] .. "   ", hl = m[2], bold = true }, { text = it.name, hl = "Normal", bold = true } }, size = style.size.body },
      { text = tostring(it.message), hl = "Dim", size = style.size.small, margin = { 2, 0, 0, 0 }, padding = { 0, 0, 0, 22 } },
    }
  end
end

function M.health()
  local mine = bone.api.health()
  for _, it in ipairs(bone._run_health()) do mine[#mine + 1] = it end
  local core
  local id
  id = bone.ui.popup({
    node = function()
      local items = { title("Health") }
      checks("This window", mine, items)
      if core then
        checks("Core", core, items)
      else
        items[#items + 1] = note("Core: checking…", { 8, 0 })
      end
      return widgets.list("health:" .. id, items, nil, { footer = hints("esc close", { 10, 0 }) })
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
