-- The desktop library's tool calls, folded the way the TUI's are
-- (bone.o.tool_detail, ctrl+t, a click): "summary" folds each run of calls
-- into one line ("Read 3 files, ran 2 shell commands"), "rows" is a card per
-- call with its output cut short (shell output to its first and last two
-- lines, other output to its first five), "full" shows everything. Edits
-- always show their diff and failures their output. A card is a title (a
-- status mark, what it did, on what) in the window's font, and below it the
-- output, diff or report in monospace.
local M = {}

local md = require("bone.desktop.markdown")
local timing = require("bone.timing")

local GUTTER_MAX = 5 -- shell output rows before the first 2, "⋮ +N", last 2
local PREVIEW_MAX = 5 -- other output rows
local EDIT_MAX = 24 -- diff rows of an edit
local AGENT_MAX = 3 -- paragraphs of a sub-agent's report
local HINT = " (click or ctrl+t)"

if not pcall(bone.o.info, "tool_detail") or not bone.o.info("tool_detail") then
  bone.o.define("tool_detail", "summary", {
    desc = "tool calls: summary, rows or full (ctrl+t)",
    choices = { "summary", "rows", "full" },
    on_change = function() bone.ui.refresh() end,
  })
end

--- What a call's title says it did, by tool name.
M.labels = {
  read_file = "Read", write_file = "Wrote", edit_file = "Edited", shell = "Ran",
  grep = "Searched", rg = "Searched", glob = "Listed", list_files = "Listed",
  web_search = "Searched the web", web_fetch = "Fetched", subagent = "Agent",
}

local KEYS = { "path", "file_path", "command", "pattern", "query", "url", "task", "prompt" }

local function args_of(item)
  return type(item.arguments) == "table" and item.arguments or {}
end

-- What the call acted on, relative to the session's folder.
local function target(item)
  local a = args_of(item)
  for _, k in ipairs(KEYS) do
    if type(a[k]) == "string" and a[k] ~= "" then
      local v = a[k]:gsub("%s+", " ")
      local s = bone.chat.session()
      local prefix = ((s and s.cwd) or ""):gsub("/$", "") .. "/"
      if #prefix > 1 and v:sub(1, #prefix) == prefix then v = v:sub(#prefix + 1) end
      return v
    end
  end
  return ""
end

-- ---- text ------------------------------------------------------------------

local function lines(text)
  local out = {}
  for line in ((text or ""):gsub("%s+$", "") .. "\n"):gmatch("(.-)\n") do out[#out + 1] = line end
  return out
end

-- Lines without blank ones at either end.
local function trimmed_lines(text)
  local ls = lines(text)
  while #ls > 0 and ls[#ls]:match("^%s*$") do ls[#ls] = nil end
  while #ls > 0 and ls[1]:match("^%s*$") do table.remove(ls, 1) end
  return ls
end

local function plural(n, noun)
  return n .. " " .. noun .. (n == 1 and "" or "s")
end

-- The shell tool ends its output with "[exit code: N]".
local function split_exit_code(text)
  local t = (text or ""):gsub("%s+$", "")
  local body, last = t:match("^(.*)\n([^\n]*)$")
  if not body then body, last = "", t end
  local code = last:match("^%[exit code: (%-?%d+)%]$")
  if not code then body = text or "" end
  if body:match("^%s*%(no output%)%s*$") then body = "" end
  return body, tonumber(code)
end

-- Stored legacy read_file rows are "LINE#HASH|text".
local function anchored(l)
  local n, text = l:match("^(%d+)#%w%w|(.*)$")
  return tonumber(n), text
end

-- read_file's output as rows { n, text, notice }: numbered lines, and the
-- notices around them (an empty file, a cut-short read).
local function read_rows(text, args)
  if text == "(empty file)" then return { { text = text, notice = true } } end
  local ls, notice = lines(text), nil
  local last = ls[#ls]
  if #ls > 1 and ls[#ls - 1] == "" and last and (
      last:match("^%[showing lines %d+%-%d+ of %d+; use offset to read more%]$")
      or last:match("^%[showing lines %d+%-%d+ of %d+; %d+ lines? not shown")
    ) then
    notice = table.remove(ls)
    table.remove(ls)
  end
  local legacy, has_anchor = true, false
  for _, l in ipairs(ls) do
    if anchored(l) then
      has_anchor = true
    elseif l ~= "" and not l:match("^%[lines %d+%-%d+ of %d+%]$") then
      legacy = false
    end
  end
  legacy = legacy and has_anchor
  local rows, next_n = {}, math.max(tonumber(args.offset) or 1, 1)
  for _, l in ipairs(ls) do
    local n, body
    if legacy then
      n, body = anchored(l)
    else
      n, body = next_n, l
      next_n = next_n + 1
    end
    rows[#rows + 1] = { n = n, text = body or l, notice = not n }
  end
  if notice then rows[#rows + 1] = { text = notice, notice = true } end
  return rows
end

local function read_summary(rows)
  local first, last, n = nil, nil, 0
  for _, r in ipairs(rows) do
    if r.n then first, last, n = first or r.n, r.n, n + 1 end
  end
  if n == 0 then return "0 lines" end
  return "lines " .. first .. "-" .. last .. " · " .. n .. " read"
end

-- Only the lines that changed: drop lines shared at the start and end.
local function diff_lines(old, new)
  local a, b = lines(old), lines(new)
  local s = 0
  while s < #a and s < #b and a[s + 1] == b[s + 1] do s = s + 1 end
  local e = 0
  while e < #a - s and e < #b - s and a[#a - e] == b[#b - e] do e = e + 1 end
  local rows = {}
  for i = s + 1, #a - e do rows[#rows + 1] = { sign = "-", text = a[i] } end
  for i = s + 1, #b - e do rows[#rows + 1] = { sign = "+", text = b[i] } end
  return rows
end

-- ---- rows ------------------------------------------------------------------
-- A body is a list of rows, each a list of spans; it is drawn as one
-- monospace block.

local function row(text, hl)
  return { { text = (text:gsub("\t", "    ")), hl = hl } }
end

local function more_row(n, noun, expanded)
  return { { text = "⋮ +" .. plural(n, noun) .. (expanded and "" or HINT), hl = "Dim" } }
end

-- The first and last two rows, "⋮ +N lines" between, unless expanded.
local function head_tail(rows, expanded)
  if expanded or #rows <= GUTTER_MAX then return rows end
  return { rows[1], rows[2], more_row(#rows - 4, "line"), rows[#rows - 1], rows[#rows] }
end

-- The first `max` rows, "⋮ +N more lines" after them, unless expanded.
local function head(rows, max, expanded)
  if expanded or #rows <= max then return rows end
  local out = {}
  for i = 1, max do out[i] = rows[i] end
  out[#out + 1] = more_row(#rows - max, "more line", expanded)
  return out
end

local function text_rows(text, hl)
  local out = {}
  for _, l in ipairs(trimmed_lines(text)) do out[#out + 1] = row(l, hl) end
  return out
end

-- Rows as a monospace block paragraph.
local function block(rows)
  local spans = {}
  for i, r in ipairs(rows) do
    if i > 1 then spans[#spans + 1] = { text = "\n", hl = "ToolOutput" } end
    for _, s in ipairs(r) do spans[#spans + 1] = s end
  end
  return { spans = spans, font = "mono", size = md.small_size - 1, bg = "CodeBlock", pad = 8, gap = 6 }
end

-- An edit's changed lines: "- old", "+ new", and a separator
-- between the edits of a batch.
local function diff_rows(args, from)
  local edits = type(args.edits) == "table" and args.edits
    or (type(args.old_string) == "string" and { args })
    or nil
  local rows, plus, minus, all = {}, 0, 0, false
  for i, e in ipairs(edits or {}) do
    if i > 1 then rows[#rows + 1] = { separator = true } end
    for _, r in ipairs(diff_lines(e.old_string or "", e.new_string or "")) do
      rows[#rows + 1] = r
      if r.sign == "+" then plus = plus + 1 else minus = minus + 1 end
    end
    all = all or e.replace_all == true
  end
  return rows, plus, minus, #(edits or {}), all
end

-- A diff as paragraphs, one per row, each a band across the whole width in
-- the theme's DiffAdd / DiffDelete colors; "···" between the edits of a
-- batch. Cut to `cap` rows unless expanded.
local function diff_paras(rows, cap, expanded)
  local shown, cut = rows, 0
  if not expanded and #rows > cap then
    shown, cut = { table.unpack(rows, 1, cap) }, #rows - cap
  end
  local size = md.small_size - 1
  local out = {}
  for i, r in ipairs(shown) do
    local p
    if r.separator then
      p = { spans = { { text = "  ···", hl = "Dim" } }, font = "mono", size = size, bg = "CodeBlock" }
    else
      local group = r.sign == "+" and "DiffAdd" or "DiffDelete"
      local text = "  " .. r.sign .. " " .. r.text:gsub("\t", "    ")
      p = { spans = { { text = text, hl = group } }, font = "mono", size = size, bg = group }
    end
    p.gap = i == 1 and 6 or 0
    out[#out + 1] = p
  end
  if cut > 0 then
    out[#out + 1] = { spans = { { text = "  ⋮ +" .. plural(cut, "line") .. HINT, hl = "Dim" } },
      font = "mono", size = size, bg = "CodeBlock" }
  end
  return out
end

-- ---- cards -----------------------------------------------------------------

local toggled = {}
local last_detail

local function detail()
  local d = bone.o.tool_detail
  d = (d == "rows" or d == "full") and d or "summary"
  if d ~= last_detail then
    toggled = {}
    last_detail = d
  end
  return d
end

-- Item keys are stable within a chat, but can repeat in another session.
local function tool_key(item)
  local s = bone.chat.session()
  return (s and s.session_id or "") .. ":" .. tostring(item.key or item.id)
end

-- A mark the proportional fonts all have.
local function mark(item)
  if not item.done then return { text = "•  ", hl = "ToolRunning" } end
  return { text = item.is_error and "×  " or "•  ", hl = item.is_error and "ToolError" or "ToolDone" }
end

--- The paragraphs of one call: its title, then its body. `expanded`: all of
--- the output, not the folded form.
local function card(item, expanded)
  local failed = item.is_error
  local a = args_of(item)
  local name = item.name
  local label = M.labels[name] or name
  local what = target(item)
  local title = { mark(item), { text = label, hl = "ToolName", bold = true } }
  if what ~= "" then
    title[#title + 1] = { text = "  " .. what, hl = name == "shell" and "ShellProgram" or "ToolPath" }
  end
  local function extra(text, hl)
    title[#title + 1] = { text = "  " .. text, hl = hl or "Dim" }
  end

  local body, paras = nil, nil -- rows as one block; or paragraphs as they are
  local out = item.output or ""
  local ok = item.done and not failed
  if failed then
    body = head_tail(text_rows(out, "ToolError"), expanded)
  elseif name == "shell" then
    local text, code = split_exit_code(item.done and out or (item.live or ""))
    if code and code ~= 0 then extra("exit " .. code, "ToolError") end
    body = head_tail(text_rows(text, code and code ~= 0 and "ToolError" or "ToolOutput"), expanded)
  elseif name == "read_file" then
    local rows = ok and read_rows(out, a) or {}
    if ok then extra(read_summary(rows)) end
    if ok and expanded then
      body = {}
      for _, r in ipairs(rows) do
        if r.n then
          body[#body + 1] = { { text = string.format("%4d  ", r.n), hl = "ToolGutter" }, { text = r.text:gsub("\t", "    "), hl = "ToolOutput" } }
        else
          body[#body + 1] = row(r.text, "Dim")
        end
      end
    end
  elseif name == "write_file" then
    local content = trimmed_lines(a.content or "")
    if ok then extra("+" .. plural(#content, "line"), "DiffAdd") end
    if ok and expanded then
      local rows = {}
      for _, l in ipairs(content) do rows[#rows + 1] = { sign = "+", text = l } end
      paras = diff_paras(rows, #rows, true)
    end
  elseif name == "edit_file" then
    local rows, plus, minus, n, all = diff_rows(a)
    if ok or not item.done then
      title[#title + 1] = { text = "  ", hl = "ToolArgs" }
      title[#title + 1] = { text = "+" .. plus, hl = "DiffAdd" }
      title[#title + 1] = { text = " -" .. minus, hl = "DiffDelete" }
      if n > 1 then extra(plural(n, "edit")) end
      if all then extra("replace all") end
      paras = diff_paras(rows, EDIT_MAX, expanded)
    end
  elseif name == "subagent" then
    if ok then
      paras = {}
      for i, p in ipairs(md.paragraphs(out, "ToolOutput")) do
        if not expanded and i > AGENT_MAX then break end
        p.size, p.indent = md.small_size, (p.indent or 0) + 14
        paras[#paras + 1] = p
      end
    end
  elseif ok then
    body = head(text_rows(out, "ToolOutput"), PREVIEW_MAX, expanded)
  end

  if item.duration_ms and item.duration_ms >= 1000 then
    extra(bone.util.duration(item.duration_ms / 1000))
  end
  local result = { { spans = title, size = md.small_size } }
  if body and #body > 0 then result[#result + 1] = block(body) end
  for _, p in ipairs(paras or {}) do result[#result + 1] = p end
  return result
end

-- ---- summaries -------------------------------------------------------------

-- Folded into a summary line: everything but edits (their diff always
-- shows) and sub-agents.
local function foldable(t)
  return t.name ~= "edit_file" and t.name ~= "subagent"
end

-- What a run of calls did, in the order first seen: "Read 3 files, ran 2
-- shell commands, called web_search 2 times".
local function summary(calls)
  local order, seen = {}, {}
  local function bump(key, phrase, unique)
    local s = seen[key]
    if not s then
      s = { phrase = phrase, n = 0, unique = {} }
      seen[key] = s
      order[#order + 1] = s
    end
    if unique then
      if not s.unique[unique] then
        s.unique[unique] = true
        s.n = s.n + 1
      end
    else
      s.n = s.n + 1
    end
  end
  for _, t in ipairs(calls) do
    local path = args_of(t).path
    if t.name == "read_file" then
      bump("read", function(n) return "read " .. plural(n, "file") end, type(path) == "string" and path or t.id)
    elseif t.name == "write_file" then
      bump("write", function(n) return "wrote " .. plural(n, "file") end, type(path) == "string" and path or t.id)
    elseif t.name == "shell" then
      bump("shell", function(n) return "ran " .. plural(n, "shell command") end)
    else
      bump("tool:" .. t.name, function(n) return "called " .. t.name .. (n > 1 and " " .. n .. " times" or "") end)
    end
  end
  local parts = {}
  for _, s in ipairs(order) do parts[#parts + 1] = s.phrase(s.n) end
  local text = table.concat(parts, ", ")
  local failed = 0
  for _, t in ipairs(calls) do failed = failed + (t.done and t.is_error and 1 or 0) end
  text = text:sub(1, 1):upper() .. text:sub(2)
  return failed > 0 and (text .. ", " .. failed .. " failed") or text
end

--- In "summary": the line for the first call of a stretch (the calls between
--- edits, reasoning aside); nil for the rest; false when this
--- call is not folded. A click on the line opens every call of it.
local function summary_paragraphs(item)
  if not item.index or not foldable(item) then return false end
  local i = item.index - 1
  local before = bone.chat.item(i)
  while before and before.kind == "reasoning" do
    i = i - 1
    before = bone.chat.item(i)
  end
  if before and before.kind == "tool" and foldable(before) then return nil end
  local stretch = {}
  for _, t in ipairs(bone.chat.items({ around = item.index, from = item.index, kind = { "tool", "reasoning" } })) do
    if t.kind == "tool" then
      if not foldable(t) then break end
      stretch[#stretch + 1] = t
    end
  end
  local running = false
  for _, t in ipairs(stretch) do running = running or not t.done end
  local paras = { { spans = {
    { text = "•  ", hl = running and "ToolRunning" or "ToolDone" },
    { text = summary(stretch), hl = "ToolArgs" },
  }, size = md.small_size } }
  if toggled[tool_key(item)] then
    for _, t in ipairs(stretch) do
      for _, p in ipairs(card(bone.chat.item(t.index, { full = true }), true)) do
        paras[#paras + 1] = p
      end
    end
  end
  return paras
end

function M.view(item, ctx)
  local mode = detail()
  local paras = false
  if mode == "summary" then paras = summary_paragraphs(item) end
  if paras == nil then return nil end
  if paras == false then
    local expanded = mode == "full"
    if toggled[tool_key(item)] then expanded = not expanded end
    paras = card(item, expanded)
  end
  local text = timing.footer(item, ctx)
  if text then
    paras[#paras + 1] = { spans = { { text, "Dim" } }, size = md.small_size, gap = 10 }
  end
  local fallback = { { { (M.labels[item.name] or item.name) .. " " .. target(item), "ToolName" } } }
  return {
    block = { paragraphs = paras, pad = { 2, 4 }, margin = { 4, 2 } },
    lines = fallback,
  }
end

-- A click on a call opens or folds it; on a summary, every call of it.
local press
bone.on("mouse", function(ev)
  local down = press
  press = ev.action == "down" and not ev.popup and not ev.panel and ev or nil
  if ev.action ~= "up" or ev.button ~= "left" or not down or down.index ~= ev.index
      or not ev.index or ev.popup or ev.panel then
    return
  end
  local item = bone.chat.item(ev.index)
  if not item or item.kind ~= "tool" then return end
  detail() -- local choices end when ctrl+t changes the level
  local key = tool_key(item)
  toggled[key] = not toggled[key] or nil
  bone.chat.redraw(ev.index)
  return true
end)

-- ctrl+t: every call one step more (or less) detailed.
local next_detail = { summary = "rows", rows = "full", full = "summary" }
bone.keymap.set("ctrl+t", function()
  bone.o.tool_detail = next_detail[bone.o.tool_detail] or "rows"
  bone.settings.set("gui.tool_detail", bone.o.tool_detail)
end)

return M
