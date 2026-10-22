-- /help: searchable commands, keyboard shortcuts and embedded documentation.
-- Commands come from the live registry, including user and plugin commands.
-- Enter prepares a command in the prompt, or opens documentation above this
-- browser; closing the document returns to the same search and selection.
-- The content (M.list, M.find) is every frontend's; M.open and M.topic
-- draw it for the TUI.

local M = {}

-- Help browser and topic lookup. Override ~/.bone/runtime/lua/bone/help.lua
-- to customize it, or replace bone.ui.help from tui.lua.
local function file(d) return d.plugin and d.name .. "/README.md" or d.name .. ".md" end

local function doc_sections()
  local out = {}
  for _, d in ipairs(bone._docs) do
    local code = false
    for line in (d.text .. "\n"):gmatch("(.-)\n") do
      if line:match("^```") then
        code = not code
      end
      local hashes, title = line:match("^(#+)%s+(.*)$")
      if hashes and not code then
        out[#out + 1] = { doc = d, level = #hashes, title = title, lines = {} }
      end
      if #out > 0 and out[#out].doc == d then
        table.insert(out[#out].lines, line)
      end
    end
  end
  return out
end

M.file = file
M.sections = doc_sections

--- Section `best` of `secs` with its subsections: text, title.
function M.section(secs, best)
  local s, lines = secs[best], {}
  for i = best, #secs do
    if i > best and (secs[i].doc ~= s.doc or secs[i].level <= s.level) then break end
    for _, l in ipairs(secs[i].lines) do lines[#lines + 1] = l end
  end
  return table.concat(lines, "\n"), file(s.doc) .. ": " .. s.title
end

--- The documentation about `topic` (a guide's name, or the section that
--- best matches): text, title; nil when there is none.
function M.find(topic)
  local t = topic:lower():match("^%s*(.-)%s*$")
  for _, d in ipairs(bone._docs) do
    if d.name:lower() == t then
      return d.text, file(d)
    end
  end
  local secs = doc_sections()
  local best, score = nil, 0
  for i, s in ipairs(secs) do
    local title = s.title:lower():gsub("`", "")
    local sc = 0
    if title == t then
      sc = 4
    elseif (" " .. title .. " "):find("[^%w_]" .. t:gsub("%p", "%%%0") .. "[^%w_]") then
      sc = 3
    elseif title:find(t, 1, true) then
      sc = 2
    elseif table.concat(s.lines, "\n"):lower():find(t, 1, true) then
      sc = 1
    end
    if sc > score then
      best, score = i, sc
    end
  end
  if not best then
    return nil
  end
  -- The section with its subsections.
  return M.section(secs, best)
end

function M.topic(topic)
  local text, title = M.find(topic)
  if not text then
    return bone.notify("no help for " .. topic .. " (try /help lua or /help usage)", "error")
  end
  return bone.ui.pager(text, { title = title })
end

-- The default shortcuts, grouped by what you are doing. Keep these in sync
-- with tui/defaults.lua; user mappings can replace the defaults.
M.KEYS = {
  { "enter", "send a message; during a turn, queue it", "Messaging" },
  { "ctrl+v / super+v / alt+v", "paste clipboard screenshot or text (Command+V on macOS if forwarded; alt+v on Windows; /paste if intercepted)", "Messaging" },
  { "alt+enter / shift+enter / ctrl+j", "insert a new line", "Messaging" },
  { "ctrl+c", "cancel the turn, clear the prompt, or press twice to quit", "Messaging" },
  { "up / down", "move in the prompt or recall message history", "Messaging" },
  { "tab", "complete the selected slash command", "Messaging" },
  { "esc", "dismiss suggestions or clear the prompt", "Messaging" },
  { "ctrl+o", "open an earlier session", "Sessions" },
  { "ctrl+n", "start a new session", "Sessions" },
  { "ctrl+d", "quit on an empty prompt", "Sessions" },
  { "f1", "open this help browser", "Sessions" },
  { "pageup / pagedown", "scroll the conversation or command suggestions", "Reading" },
  { "shift+up / shift+down / wheel", "scroll the conversation or command suggestions", "Reading" },
  { "ctrl+home / ctrl+end", "jump to the top or bottom; bottom follows new output", "Reading" },
  { "ctrl+r", "show or hide model reasoning", "Reading" },
  { "ctrl+t", "cycle tool detail: summary, rows, full", "Reading" },
  { "mouse drag", "select text; release to copy to the clipboard", "Reading" },
  { "down on an empty prompt", "focus queued messages, sub-agents and shell jobs", "Tray" },
  { "up on an empty prompt", "edit the last queued message", "Tray" },
  { "ctrl+b", "fold or expand the tray", "Tray" },
  { "tab in the tray", "switch between Queue, Agents and Jobs", "Tray" },
  { "enter in the tray", "edit a queued message, open an agent or a shell terminal", "Tray" },
  { "c in the tray", "cancel the selected agent or shell job", "Tray" },
  { "s in Queue", "switch steer/next; when idle, send now", "Tray" },
  { "shift+up / shift+down in Queue", "reorder queued messages", "Tray" },
  { "d / r in Queue", "drop the selected message / resume a paused queue", "Tray" },
  { "esc in a sub-agent", "return to the main session when the prompt is empty", "Tray" },
  { "ctrl+left / ctrl+right", "move by word (also alt+left / alt+right)", "Editing" },
  { "home / end", "start or end of line (also ctrl+a / ctrl+e)", "Editing" },
  { "ctrl+w / alt+backspace", "delete the word before the cursor", "Editing" },
  { "ctrl+u / ctrl+k", "delete to the start or end of the line", "Editing" },
  { "ctrl+p", "move focus to the next panel", "Panels" },
  { "tab / shift+tab in a panel", "move to the next or previous panel", "Panels" },
  { "esc in a panel", "return focus to the prompt", "Panels" },
}

--- The tabs of the browser.
M.TABS = { "Commands", "Keys", "Docs" }

--- The entries of tab `tab` (1 commands, 2 keys, 3 docs) that match every
--- word of `query`: { label, desc, meta, command | section | doc | topic }.
function M.list(tab, query)
  local docs = {}
  local sections = doc_sections()
  for _, d in ipairs(bone._docs) do
    docs[#docs + 1] = d.plugin
      and { label = d.name, desc = "Read the " .. d.name .. " plugin's guide", doc = d, meta = "Plugin" }
      or { label = d.name .. ".md", desc = "Read the complete " .. d.name .. " guide", topic = d.name, meta = "Full guide" }
  end
  for i, s in ipairs(sections) do
    docs[#docs + 1] = { label = s.title:gsub("`", ""), desc = "Read this section of " .. file(s.doc), meta = file(s.doc), section = i }
  end
  local keys, guide = {}, {}
  for _, k in ipairs(M.KEYS) do
    keys[#keys + 1] = { label = k[1], desc = k[2], meta = k[3] .. " · default shortcut", topic = "keys" }
  end
  -- Mappings given a desc (plugins' and your own).
  for _, d in ipairs(bone._docs) do if d.plugin then guide[d.plugin] = d end end
  local mine = bone.keymap.list()
  table.sort(mine, function(a, b) return (a.plugin or "") .. "\0" .. a.keys < (b.plugin or "") .. "\0" .. b.keys end)
  for _, k in ipairs(mine) do
    local ctx = k.context ~= "main" and " · in " .. k.context or ""
    keys[#keys + 1] = { label = k.keys, desc = k.desc, meta = (k.plugin or "your config") .. ctx, doc = guide[k.plugin or ""], topic = "keys" }
  end

  local all = {}
  do
    if tab == 1 then
      for _, c in ipairs(bone.cmd.list()) do
        local aliases = #c.aliases > 0 and ("aliases: " .. table.concat(c.aliases, ", ")) or "Slash command"
        all[#all + 1] = { label = "/" .. c.name, desc = c.desc or "", meta = aliases, command = c.name }
      end
    elseif tab == 2 then
      all = keys
    else
      all = docs
    end
  end
  local out = {}
  for _, it in ipairs(all) do
    local text = (it.label .. " " .. it.desc .. " " .. it.meta):lower()
    local match = true
    for word in (query or ""):lower():gmatch("%S+") do
      if not text:find(word, 1, true) then
        match = false
        break
      end
    end
    if match then out[#out + 1] = it end
  end
  return out
end

--- Open entry `it` of M.list: a command goes in the prompt; documentation
--- goes to read(text, title). Returns whether the browser should close.
function M.choose(it, read)
  if it and it.command then
    bone.prompt.set("/" .. it.command .. " ")
    return true
  elseif it and it.section then
    -- Use this exact section, rather than searching a duplicate heading.
    read(M.section(doc_sections(), it.section))
  elseif it and it.doc then
    read(it.doc.text, file(it.doc))
  elseif it then
    local text, title = M.find(it.topic)
    if text then read(text, title) end
  end
  return false
end

function M.open()
  local st = { tab = 1, query = "", sel = 1, rows = 1 }
  local tabs = M.TABS
  local function matches()
    return M.list(st.tab, st.query)
  end

  -- Clip spans before boxing so long descriptions and narrow terminals
  -- never wrap the chrome and displace the footer or selected row.
  local function clip(spans, width)
    local out, left = {}, width
    for _, span in ipairs(spans) do
      if left <= 0 then break end
      local text = bone.text.truncate(span[1], left)
      out[#out + 1] = { text, span[2] }
      left = left - bone.text.width(text)
    end
    return out
  end

  local function render(ctx)
    local w = math.min(100, math.max(4, ctx.width - 4))
    local h = math.min(28, math.max(1, ctx.height - 2))
    local inner = w - 4
    if h < 8 or inner < 12 then
      return { clip({ { "Help · esc close", "Accent" } }, ctx.width) }
    end
    local list = matches()
    st.sel = math.max(1, math.min(st.sel, math.max(1, #list)))
    local tabline = {}
    for i, tab in ipairs(tabs) do
      if inner < 28 then tab = ({ "Cmds", "Keys", "Docs" })[i] end
      tabline[#tabline + 1] = { " " .. tab .. " ", st.tab == i and "Accent" or "Dim" }
      if i < #tabs then tabline[#tabline + 1] = { "│", "PopupBorder" } end
    end
    local body = {
      clip(tabline, inner),
      clip({ { "Search  ", "Accent" }, { st.query == "" and "type to filter…" or st.query, st.query == "" and "Dim" or "Normal" }, { "▏", "Accent" } }, inner),
      { { string.rep("─", inner), "PopupBorder" } },
    }
    local details = h >= 16 and 4 or 0
    st.rows = h - 2 - #body - details - 1
    local first = math.max(1, st.sel - st.rows + 1)
    local label_w = math.min(28, math.floor(inner * 0.4))
    if #list == 0 then
      body[#body + 1] = clip({ { "No matches. ctrl+u clears the search.", "Dim" } }, inner)
    else
      for i = first, math.min(#list, first + st.rows - 1) do
        local it, selected = list[i], i == st.sel
        local label = bone.text.truncate(it.label, label_w)
        local row = clip({
          { selected and "› " or "  ", selected and "Accent" or "Dim" },
          { label .. string.rep(" ", label_w - bone.text.width(label)), selected and "Accent" or "Normal" },
          { "  " .. (st.tab == 3 and it.meta or it.desc), "Dim" },
        }, inner)
        if selected then
          local text = ""
          for _, span in ipairs(row) do text = text .. span[1] end
          row = { { text .. string.rep(" ", inner - bone.text.width(text)), "Selection" } }
        end
        body[#body + 1] = row
      end
    end
    while #body < 3 + st.rows do body[#body + 1] = {} end
    if details > 0 then
      local it = list[st.sel]
      body[#body + 1] = clip({ { it and (it.label .. " ") or "", "Accent" }, { string.rep("─", inner), "PopupBorder" } }, inner)
      local description = it and it.desc or "Try a command name, alias, shortcut or documentation topic."
      local wrapped = bone.text.wrap({ { description, "Normal" } }, inner)
      body[#body + 1] = wrapped[1] or {}
      body[#body + 1] = wrapped[2] or {}
      local meta = it and it.meta or ""
      if it and it.command then meta = meta .. " · enter puts it in the prompt" end
      if st.tab == 2 then meta = meta .. (it and it.doc and " · enter opens its guide" or " · enter opens the keys guide") end
      local pos = #list > 0 and (st.sel .. "/" .. #list) or "0 matches"
      body[#body + 1] = clip({ { pos .. "  ·  " .. meta, "Dim" } }, inner)
    end
    body[#body + 1] = clip({ { inner >= 68 and "tab/←→ tabs · ↑↓ move · enter choose · ctrl+u clear · esc close" or "tab tabs · ↑↓ move · enter · esc", "Dim" } }, inner)
    return bone.ui.box(body, { title = "bone / help", width = w })
  end

  local id
  local function on_key(k)
    local list = matches()
    if k == "esc" then
      bone.ui.close(id)
    elseif k == "tab" or k == "right" or k == "shift+tab" or k == "backtab" or k == "left" then
      local by = (k == "tab" or k == "right") and 1 or -1
      st.tab = (st.tab - 1 + by) % #tabs + 1
      st.query, st.sel = "", 1
    elseif k == "up" or k == "ctrl+p" or k == "wheelup" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "ctrl+n" or k == "wheeldown" then
      st.sel = math.min(math.max(1, #list), st.sel + 1)
    elseif k == "pageup" or k == "pagedown" then
      st.sel = math.max(1, math.min(math.max(1, #list), st.sel + (k == "pageup" and -st.rows or st.rows)))
    elseif k == "home" then
      st.sel = 1
    elseif k == "end" then
      st.sel = math.max(1, #list)
    elseif k == "enter" then
      local close = M.choose(list[st.sel], function(text, title)
        bone.ui.pager(text, { title = title })
      end)
      if close then bone.ui.close(id) end
    elseif bone.text.edit(st.query, k) then
      st.query, st.sel = bone.text.edit(st.query, k), 1
    else
      return false
    end
    return true
  end
  id = bone.ui.popup({ lines = render, on_key = on_key })
  return id
end

return M
