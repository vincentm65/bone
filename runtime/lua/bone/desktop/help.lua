-- The desktop library's help (/help, f1), document reader and /health, as
-- windows in the middle of the screen. Help: Commands, Keys and Docs
-- (click or tab), a search field (type to filter), each entry as a card
-- with what it does; enter puts a command in the prompt or opens its
-- documentation in the reader, which lays Markdown out as the chat does.
-- The content is bone.help's.
local M = {}

local help = require("bone.help")
local md = require("bone.desktop.markdown")
local card = require("bone.desktop.card")

local function title(text)
  return { paragraphs = { { spans = { { text = text, hl = "Normal", bold = true } }, size = 20 } }, pad = { 4, 10 }, margin = { 0, 6 } }
end

local function note(text, margin)
  return { paragraphs = { { spans = { { text, "Dim" } }, size = 12 } }, pad = { 4, 10 }, margin = margin or { 10, 0 } }
end

--- Read Markdown `text` in a window titled `name`; esc closes it.
function M.read(text, name)
  local id
  local w, h = card.size()
  id = bone.ui.popup({
    width = w, height = h,
    blocks = function()
      local out = { { id = "title", block = title(name or "Help") } }
      local paras = md.paragraphs(text or "")
      out[#out + 1] = { id = "text", block = { paragraphs = paras, size = md.text_size, pad = { 0, 10 } } }
      out[#out + 1] = { id = "keys", block = note("wheel  scroll     esc  close") }
      return out
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
  local function blocks()
    list = help.list(st.tab, st.query)
    st.sel = math.max(1, math.min(st.sel, math.max(1, #list)))
    local out = { { id = "title", block = title("Help") } }
    local tabs = {}
    for i, t in ipairs(help.TABS) do
      if i > 1 then tabs[#tabs + 1] = { text = "        ", hl = "Dim" } end
      tabs[#tabs + 1] = { text = t, hl = i == st.tab and "Normal" or "Dim", bold = i == st.tab }
    end
    out[#out + 1] = { id = "tabs", block = { paragraphs = { { spans = tabs, size = 14 } }, pad = { 2, 10 }, margin = { 0, 8 } } }
    out[#out + 1] = { id = "search", block = {
      paragraphs = { { spans = { { st.query == "" and "Type to search…" or st.query, st.query == "" and "Dim" or "Normal" } }, size = 13 } },
      bg = "InputBackground", radius = 10, pad = { 7, 12 }, margin = { 0, 8 }, fill = true,
    } }
    if #list == 0 then
      out[#out + 1] = { id = "none", block = note("No matches.", { 4, 4 }) }
    end
    for i, it in ipairs(list) do
      local on = i == st.sel
      local paras = {
        { spans = { { text = it.label, hl = it.command and "Accent" or "Normal", bold = on } }, size = 14 },
        { spans = { { st.tab == 3 and it.meta or it.desc, "Dim" } }, size = 12, gap = 3 },
      }
      if on and st.tab ~= 3 and it.meta ~= "" then
        paras[#paras + 1] = { spans = { { it.meta, "Dim" } }, size = 11, gap = 3 }
      end
      out[#out + 1] = { id = i, selected = on, block = {
        paragraphs = paras, bg = on and "Selection" or nil, radius = 10, pad = { 7, 12 }, margin = { 1, 1 }, fill = true,
      } }
    end
    out[#out + 1] = { id = "keys", block = note("tab  section     ↑↓  move     enter  open     esc  close") }
    return out
  end
  local function choose(i)
    if help.choose(list[i], M.read) then bone.ui.close(id) end
  end
  local function on_key(k)
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
  id = bone.ui.popup({
    width = w, height = h, blocks = blocks, on_key = on_key,
    on_block = function(bid)
      if bid == "tabs" then
        on_key("tab")
      elseif type(bid) == "number" then
        if st.sel == bid then choose(bid) else st.sel = bid end
        bone.ui.update(id, {})
      end
    end,
  })
  return id
end

-- /health: this window's checks, then the core's.
local MARK = { ok = { "•", "ToolDone" }, warn = { "!", "WarningMsg" }, error = { "×", "ErrorMsg" } }
local function checks(name, items, out)
  out[#out + 1] = { id = name, block = {
    paragraphs = { { spans = { { text = name, hl = "Normal", bold = true } }, size = 15 } }, pad = { 2, 10 }, margin = { 8, 4 },
  } }
  for i, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = { id = name .. i, block = {
      paragraphs = {
        { spans = { { text = m[1] .. "   ", hl = m[2], bold = true }, { text = it.name, hl = "Normal", bold = true } }, size = 14 },
        { spans = { { tostring(it.message), "Dim" } }, size = 12, gap = 2, indent = 22 },
      },
      bg = "Panel", radius = 10, pad = { 7, 12 }, margin = { 1, 1 }, fill = true,
    } }
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
    blocks = function()
      local out = { { id = "title", block = title("Health") } }
      checks("This window", mine, out)
      if core then
        checks("Core", core, out)
      else
        out[#out + 1] = { id = "wait", block = note("Core: checking…", { 8, 0 }) }
      end
      out[#out + 1] = { id = "keys", block = note("esc  close") }
      return out
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
