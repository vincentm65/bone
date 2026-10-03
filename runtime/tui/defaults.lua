-- Default keys. ~/.bone/tui.lua runs after this and can change or delete
-- any of them; ~/.bone/runtime/tui/defaults.lua replaces this file.
-- Keys without a mapping type text into the prompt.

local function map(keys, context)
  for key, action in pairs(keys) do
    bone.keymap.set(key, action, { context = context })
  end
end

-- The prompt (the "main" context).
map({
  ["enter"] = "submit",
  ["alt+enter"] = "newline",
  ["shift+enter"] = "newline",
  ["ctrl+j"] = "newline",
  ["tab"] = "complete",
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["ctrl+d"] = "quit_if_empty",
  ["ctrl+r"] = "sessions",
  ["ctrl+n"] = "new_session",

  ["left"] = "left",
  ["right"] = "right",
  ["up"] = "up",
  ["down"] = "down",
  ["ctrl+left"] = "word_left",
  ["ctrl+right"] = "word_right",
  ["alt+left"] = "word_left",
  ["alt+right"] = "word_right",
  ["home"] = "line_start",
  ["end"] = "line_end",
  ["ctrl+a"] = "line_start",
  ["ctrl+e"] = "line_end",
  ["backspace"] = "backspace",
  ["ctrl+h"] = "backspace",
  ["delete"] = "delete",
  ["ctrl+w"] = "delete_word",
  ["alt+backspace"] = "delete_word",
  ["ctrl+u"] = "delete_to_start",
  ["ctrl+k"] = "delete_to_end",

  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
  ["shift+up"] = "scroll_up",
  ["shift+down"] = "scroll_down",
  ["ctrl+home"] = "scroll_top",
  ["ctrl+end"] = "scroll_bottom",
})

-- While a Lua popup is open (after the popup's own keys).
map({
  ["ctrl+c"] = "interrupt",
}, "popup")

-- While a panel has the keyboard (after the panel's own keys). Scrolling and
-- dismiss act on the panel; text that is not mapped is ignored.
map({
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["tab"] = "focus_next",
  ["shift+tab"] = "focus_prev",
  ["up"] = "up",
  ["down"] = "down",
  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  ["home"] = "scroll_top",
  ["end"] = "scroll_bottom",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
}, "panel")

-- One blank row between the chat and the prompt, so text never touches
-- what you type. Replace it (a spinner line, a rule) or set it to nil.
function bone.ui.divider()
  return {}
end

-- The session picker (ctrl+r, /sessions), built on bone.ui.select.
function bone.ui.sessions()
  local home = os.getenv("HOME")
  local picker = bone.ui.select({}, {
    prompt = "Sessions",
    loading = true,
    empty = "no sessions yet",
    format = function(s)
      local dir = s.cwd
      if home and dir:sub(1, #home) == home then
        dir = "~" .. dir:sub(#home + 1)
      end
      return (s.title or "[untitled]") .. "  " .. dir
    end,
    on_choice = function(s)
      if s then
        bone.api.open_session(s.session_id)
      end
    end,
  })
  bone.request("session/list", {}, function(list, err)
    if err then
      picker:close()
      bone.notify("cannot list sessions: " .. err, "error")
      return
    end
    picker:set_items(list)
  end)
end

-- Matching slash commands while typing "/…", right above the prompt.
-- ctx = { items = { { name, desc } }, selected, width, height }.
function bone.ui.suggestions(ctx)
  local name_w, desc_w = 0, 0
  for _, it in ipairs(ctx.items) do
    name_w = math.max(name_w, bone.text.width(it.name) + 1)
    desc_w = math.max(desc_w, bone.text.width(it.desc))
  end
  local lines = {}
  for i, it in ipairs(ctx.items) do
    local hl = i == ctx.selected and "Selection" or "Normal"
    local name = "/" .. it.name
    lines[i] = {
      { name .. string.rep(" ", name_w + 1 - bone.text.width(name)), hl },
      { it.desc .. string.rep(" ", desc_w - bone.text.width(it.desc)), hl },
    }
  end
  return bone.ui.box(lines, { border_hl = "WinSeparator", width = math.min(name_w + desc_w + 5, ctx.width) })
end

-- /help topic: the best matching section of the docs, in a pager.
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
        out[#out + 1] = { doc = d.name, level = #hashes, title = title, lines = {} }
      end
      if #out > 0 and out[#out].doc == d.name then
        table.insert(out[#out].lines, line)
      end
    end
  end
  return out
end

function bone.ui.help(topic)
  local t = topic:lower()
  for _, d in ipairs(bone._docs) do
    if d.name == t then
      return bone.ui.pager(d.text, { title = d.name .. ".md" })
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
    return bone.notify("no help for " .. topic .. " (try /help lua or /help usage)", "error")
  end
  -- The section with its subsections.
  local s = secs[best]
  local lines = {}
  for i = best, #secs do
    if i > best and (secs[i].doc ~= s.doc or secs[i].level <= s.level) then
      break
    end
    for _, l in ipairs(secs[i].lines) do
      lines[#lines + 1] = l
    end
  end
  bone.ui.pager(table.concat(lines, "\n"), { title = s.doc .. ".md: " .. s.title })
end

-- /health: the TUI's checks, then the core's (over the protocol).
local MARK = { ok = { "✓ ", "DiffAdd" }, warn = { "! ", "WarningMsg" }, error = { "✗ ", "ErrorMsg" } }

local function health_lines(title, items, out)
  out[#out + 1] = { { title, "Accent" } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = { m, { it.name .. ": ", "Normal" }, { it.message, "Dim" } }
  end
  out[#out + 1] = {}
end

function bone.ui.health()
  local tui = bone.api.health()
  for _, it in ipairs(bone._run_health()) do
    tui[#tui + 1] = it
  end
  local lines = {}
  health_lines("TUI", tui, lines)
  local pager = bone.ui.pager({ unpack(lines), { { "Core: checking…", "Dim" } } }, { title = "Health" })
  bone.request("health/check", {}, function(core, err)
    local all = { unpack(lines) }
    if err then
      health_lines("Core", { { name = "core", status = "error", message = err } }, all)
    else
      health_lines("Core", core or {}, all)
    end
    pager:set(all)
  end)
end

-- Colors (see runtime/colors/).
bone.colorscheme("black")
