-- The desktop library's settings (/config, /settings, /model): a window in
-- the middle of the screen, as a card. Its heading says where you are,
-- then the section (General, Providers, Plugins; click or tab to change),
-- each setting as a card with its name, its value and what it does (the
-- selected one raised), and at the bottom what happened and the keys. The
-- page's state and actions are bone.settings_page; options are saved as
-- gui.<name>. Opened in place of bone.config, so every way in comes here.
local M = {}

local SIZE, SMALL = 14, 12

local function value_text(row, st)
  local function onoff(on) return on and { "On", "ToolDone" } or { "Off", "Dim" } end
  if row.kind == "provider" then
    local t = (row.model or "") .. (row.url and row.url ~= "" and ("  ·  " .. row.url) or "")
    if row.current then return { t .. "  ·  in use", "Accent" } end
    return { t, "Dim" }
  elseif row.kind == "paction" then
    return nil
  elseif row.kind == "page" then
    return { "Open  ›", "Accent" }
  elseif row.kind == "plugin" then
    local where = (row.tui and "GUI" or "") .. (row.tui and row.core and " + " or "") .. (row.core and "core" or "")
    local v = onoff(row.on)
    return { v[1] .. (where ~= "" and ("  ·  " .. where) or "") .. (row.error and ("  ·  " .. row.error) or ""),
      row.error and "ErrorMsg" or v[2] }
  elseif row.type == "boolean" then
    return onoff(row.value)
  elseif row.type == "secret" and st.prov and st.prov.new then
    return { row.value ~= "" and string.rep("•", math.min(#row.value, 24)) or "None", "Dim" }
  elseif row.choices and #row.choices > 0 then
    local out = {}
    for _, c in ipairs(row.choices) do
      out[#out + 1] = c == row.value and ("[" .. tostring(c) .. "]") or tostring(c)
    end
    return { table.concat(out, "   "), "Normal" }
  elseif row.key == "type" and (row.value == nil or row.value == "") then
    return { "OpenAI-compatible", "Normal" }
  elseif row.value == nil or row.value == "" then
    return { row.kind == "option" and "Unset" or "None", "Dim" }
  end
  return { tostring(row.value) .. (row.changed and "   (changed)" or ""), "Normal" }
end

local function hints(st, t)
  if st.ask then return "y  yes     n  no" end
  if st.edit then return "enter  save     esc  cancel" end
  if t.page or st.prov then return "↑↓  move     enter  change     r  reset     esc  back" end
  if t.name == "providers" then return "↑↓  move     enter  use     e  edit     a  add     d  delete     tab  section     esc  close" end
  if t.name == "plugins" then return "↑↓  move     enter  settings     space  on/off     tab  section     esc  close" end
  return "↑↓  move     enter  change     r  reset     tab  section     esc  close"
end

function M.open(want)
  local id, paste_event
  local page
  page = require("bone.settings_page").new(want, {
    scope = "gui",
    changed = function() if id then bone.ui.update(id, {}) end end,
    close = function()
      if paste_event then bone.off(paste_event) paste_event = nil end
      bone.ui.close(id)
    end,
  })
  local st = page.st

  local function blocks()
    local out = {}
    local function add(bid, block, selected) out[#out + 1] = { id = bid, block = block, selected = selected } end
    local t = st.tabs[st.tab]
    local section = t.section or st.tab
    local where = "Settings"
    if t.page then
      where = where .. "  ›  " .. st.tabs[section].title .. "  ›  " .. t.title
    elseif st.prov then
      where = where .. "  ›  Providers  ›  " .. (st.prov.new and "New provider" or st.prov.name)
    end
    add("title", { paragraphs = { { spans = { { text = where, hl = "Normal", bold = true } }, size = 20 } },
      pad = { 4, 10 }, margin = { 0, 4 } })
    local tabs = {}
    for i = 1, 3 do
      local tab = st.tabs[i]
      if i > 1 then tabs[#tabs + 1] = { text = "        ", hl = "Dim" } end
      tabs[#tabs + 1] = { text = tab.title, hl = i == section and "Normal" or "Dim", bold = i == section }
    end
    add("tabs", { paragraphs = { { spans = tabs, size = SIZE } }, pad = { 2, 10 }, margin = { 0, 10 } })

    local list = page.rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    if t.name == "providers" and not st.providers then
      add("none", { paragraphs = { { spans = { { "Loading…", "Dim" } }, size = SIZE } }, pad = { 6, 10 } })
    elseif #list == 0 then
      local empty = t.name == "plugins" and "No plugins in ~/.bone/plugins" or "Nothing to set here"
      add("none", { paragraphs = { { spans = { { empty, "Dim" } }, size = SIZE } }, pad = { 6, 10 } })
    end
    for i, r in ipairs(list) do
      local on = i == st.sel
      local paras = { { spans = { { text = r.label, hl = "Normal", bold = on } }, size = SIZE } }
      if on and st.edit then
        local typed = r.type == "secret" and string.rep("•", #st.edit) or st.edit
        paras[#paras + 1] = { spans = { { typed .. "|", "Accent" } }, size = SIZE - 1, gap = 3 }
      else
        local v = value_text(r, st)
        if v then paras[#paras + 1] = { spans = { v }, size = SIZE - 1, gap = 3 } end
        if r.kind == "option" then
          local saved = bone.settings.get(r.path)
          if saved ~= nil and saved ~= r.value then
            paras[#paras + 1] = { spans = { { "gui.lua sets " .. tostring(r.value), "Dim" } }, size = SMALL, gap = 2 }
          end
        end
      end
      if on and r.desc and r.desc ~= "" then
        paras[#paras + 1] = { spans = { { r.desc, "Dim" } }, size = SMALL, gap = 4 }
      end
      add(i, { paragraphs = paras, bg = on and "Selection" or nil, radius = 10, pad = { 8, 12 },
        margin = { 1, 1 }, fill = true }, on)
    end

    local note
    if st.ask then
      note = { st.ask.text, "Accent" }
    elseif st.note then
      note = { st.note[1], st.note[2] == "ErrorMsg" and "ErrorMsg" or "Dim" }
    end
    if note then
      add("note", { paragraphs = { { spans = { note }, size = SIZE - 1 } }, pad = { 4, 10 }, margin = { 10, 0 } })
    end
    add("keys", { paragraphs = { { spans = { { hints(st, t), "Dim" } }, size = SMALL } }, pad = { 4, 10 },
      margin = { note and 2 or 10, 0 } })
    return out
  end

  local function on_block(bid)
    if bid == "tabs" and not st.prov and st.tab <= 3 then
      page.switch_tab(1)
    elseif type(bid) == "number" then
      local list = page.rows()
      if st.sel == bid then
        page.activate(list[bid], false)
      else
        st.sel = bid
      end
    end
    bone.ui.update(id, {})
  end

  -- As big as the window allows, up to a comfortable size.
  local cols, rows = 100, 34
  local ok, geo = pcall(bone.ui.geometry)
  if ok and geo then
    local w, h = 0, 0
    for _, l in ipairs(geo.leaves or {}) do
      w, h = math.max(w, l.x + l.width), math.max(h, l.y + l.height)
    end
    if w > 0 then cols = math.min(cols, w - 8) end
    if h > 0 then rows = math.min(rows, h - 4) end
  end
  id = bone.ui.popup({
    blocks = blocks, on_block = on_block, on_key = page.on_key,
    width = math.max(40, cols), height = math.max(12, rows),
  })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" then page.paste(ev.text) end
  end)
  return id
end

return M
