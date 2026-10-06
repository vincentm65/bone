-- How the transcript looks. Rust hands each item over as data and draws
-- whatever lines come back; all spacing, prefixes and colors are here.
-- Override any of these from tui.lua, e.g.:
--   bone.ui.views.reasoning = function(item, ctx) return nil end   -- hide it
--
-- item: { kind, index, text, streaming }           user/reasoning/assistant
--       { kind, index, text, error }                notice
--       { kind, index, id, name, arguments, raw_arguments,
--         output, is_error, done }                   tool
--       { kind, index, id, text, mode, position }  queued (mode: "steer" or "next")
-- ctx:  { width, region, prev = { kind } }
-- Returns a list of lines; each line is a list of { "text", "Group" }.

local views = bone.ui.views
local wrap = bone.text.wrap

local function append(out, rows)
  for _, r in ipairs(rows) do
    out[#out + 1] = r
  end
  return out
end

-- Like Rust's str::lines: split on "\n", no trailing empty line.
local function lines(s)
  local t = {}
  if s == nil or s == "" then
    return t
  end
  for l in (s .. "\n"):gmatch("(.-)\n") do
    t[#t + 1] = l
  end
  if s:sub(-1) == "\n" then
    t[#t] = nil
  end
  return t
end

local function trim(s)
  return (s:gsub("^%s+", ""):gsub("%s+$", ""))
end

-- A blank line between exchanges.
local function starts(kind, ctx)
  if not ctx.prev then
    return {}
  end
  return { {} }
end

-- ---- markdown -------------------------------------------------------------

local function md_spans(spans, base)
  local out = {}
  for _, s in ipairs(spans) do
    if s.code then
      out[#out + 1] = { s.text, "MdCode" }
    elseif s.link then
      out[#out + 1] = { s.text, "MdLink" }
      if s.link ~= s.text then
        out[#out + 1] = { " <" .. s.link .. ">", "Dim" }
      end
    elseif s.bold then
      out[#out + 1] = { s.text, "MdBold" }
    elseif s.italic then
      out[#out + 1] = { s.text, "MdItalic" }
    else
      out[#out + 1] = { s.text, base }
    end
  end
  return out
end

-- Lay a table block out to fixed-width lines. Column widths come from the
-- widest cell; if the table is wider than `width`, the rightmost columns are
-- truncated with an ellipsis. Cells are plain spans (no wrapping).
local function md_table(b, width, indent)
  local max_cols = 0
  for _, row in ipairs(b.rows) do
    max_cols = math.max(max_cols, #row)
  end
  local w = {}
  for c = 1, max_cols do
    w[c] = 0
    for _, row in ipairs(b.rows) do
      local cell = row[c]
      if cell then
        local n = 0
        for _, s in ipairs(cell) do
          n = n + bone.text.width(s.text)
        end
        w[c] = math.max(w[c], n)
      end
    end
  end
  -- separators: one " │ " (3 wide) between columns.
  local seps = math.max(0, max_cols - 1) * 3
  local total = bone.text.width(indent) + seps
  for c = 1, max_cols do
    total = total + w[c]
  end
  if total > width then
    -- Truncate from the right; each kept column stays at least 2 wide.
    local budget = width - bone.text.width(indent) - seps
    for c = max_cols, 1, -1 do
      local rest = 0
      for k = 1, c - 1 do
        rest = rest + math.min(w[k], 1)
      end
      local room = math.max(1, budget - rest)
      w[c] = math.min(w[c], room)
      budget = budget - w[c]
    end
  end
  local out = {}
  for i, row in ipairs(b.rows) do
    local is_head = b.header and i == 1
    local hl = is_head and "MdTableHeader" or "MdTable"
    local line = { { indent, "Normal" } }
    for c = 1, max_cols do
      local cell = row[c] or {}
      local spans = bone.text.clip(md_spans(cell, hl), w[c])
      local n = 0
      for _, s in ipairs(spans) do
        n = n + bone.text.width(s[1])
      end
      local pad = math.max(0, w[c] - n)
      local align = b.align and b.align[c]
      local left = align == "r" and pad or align == "c" and math.floor(pad / 2) or 0
      line[#line + 1] = { string.rep(" ", left), hl }
      for _, s in ipairs(spans) do
        line[#line + 1] = s
      end
      line[#line + 1] = { string.rep(" ", pad - left), hl }
      if c < max_cols then
        line[#line + 1] = { " │ ", "Dim" }
      end
    end
    out[#out + 1] = line
  end
  return out
end

--- Markdown text to lines, every row prefixed with `indent`.
function bone.ui.markdown(text, width, indent)
  indent = indent or ""
  local pre = { { indent, "Normal" } }
  local out = {}
  for _, b in ipairs(bone.markdown.parse(text)) do
    if b.kind == "blank" then
      out[#out + 1] = {}
    elseif b.kind == "heading" then
      append(out, wrap(md_spans(b.spans, "MdHeading"), width, { first = pre, rest = pre }))
    elseif b.kind == "rule" then
      out[#out + 1] = { { indent, "Normal" }, { string.rep("─", math.min(width - #indent, 40)), "Dim" } }
    elseif b.kind == "quote" then
      local p = { { indent, "Normal" }, { "▏ ", "MdQuote" } }
      append(out, wrap(md_spans(b.spans, "MdQuote"), width, { first = p, rest = p }))
    elseif b.kind == "table" then
      append(out, md_table(b, width, indent))
    elseif b.kind == "item" then
      local lead = string.rep(" ", b.indent)
      local bullet = b.ordered and (b.marker .. " ") or "• "
      local first = { { indent .. lead, "Normal" }, { bullet, "MdBullet" } }
      local rest = { { string.rep(" ", #indent + #lead + bone.text.width(bullet)), "Normal" } }
      append(out, wrap(md_spans(b.spans, "Normal"), width, { first = first, rest = rest }))
    elseif b.kind == "code" then
      local p = { { indent, "Normal" }, { "▎ ", "ToolGutter" } }
      for _, l in ipairs(b.lines) do
        append(out, wrap({ { l, "MdCodeBlock" } }, width, { first = p, rest = p }))
      end
    else
      local first = { { indent .. string.rep(" ", b.indent), "Normal" } }
      append(out, wrap(md_spans(b.spans, "Normal"), width, { first = first, rest = pre }))
    end
  end
  return out
end

-- ---- messages -------------------------------------------------------------

function views.user(item, ctx)
  local out = starts("user", ctx)
  local band = "UserMessage"
  for i, line in ipairs(lines(item.text)) do
    local first = i == 1 and { { "› ", "UserPrompt" } } or { { "  ", band } }
    append(out, wrap({ { line, band } }, ctx.width, { first = first, rest = { { "  ", band } }, pad = band }))
  end
  return out
end

function views.reasoning(item, ctx)
  if not bone.o.show_reasoning or trim(item.text) == "" then
    return nil
  end
  local out = starts("reasoning", ctx)
  local prefix = item.streaming and "∴ " or "│ "
  for _, line in ipairs(lines(trim(item.text))) do
    append(out, wrap({ { line, "Reasoning" } }, ctx.width,
      { first = { { prefix, "Reasoning" } }, rest = { { "  ", "Reasoning" } } }))
  end
  return out
end

function views.assistant(item, ctx)
  local out = starts("assistant", ctx)
  local text = trim(item.text)
  if text == "" then
    if item.streaming then
      out[#out + 1] = { { "  …", "Dim" } }
    end
    return out
  end
  local md = bone.ui.markdown(text, ctx.width, "  ")
  if item.streaming then
    if #md > 0 then
      table.insert(md[#md], { "▍", "Dim" })
    else
      md[1] = { { "  ▍", "Dim" } }
    end
  end
  return append(out, md)
end

function views.notice(item, ctx)
  local hl = item.error and "ErrorMsg" or "Notice"
  return append(starts("notice", ctx), wrap({ { item.text, hl } }, ctx.width, { first = { { "  ! ", hl } } }))
end

-- A compaction (see /compact): one line; the chat above it stays.
function views.compacted(item, ctx)
  return append(starts("compacted", ctx), wrap({ { item.text or "", "Dim" } }, ctx.width,
    { first = { { "◇ ", "Accent" } }, rest = { { "  ", "Dim" } } }))
end

function views.queued(item, ctx)
  local s = bone.chat.session()
  local note = s and s.queue_paused and "paused · /queue resume" or item.mode == "steer" and "joins this turn" or "next turn"
  local out = starts("queued", ctx)
  for i, line in ipairs(lines(item.text)) do
    local first = i == 1 and { { "◦ ", "Dim" } } or { { "  ", "Dim" } }
    append(out, wrap({ { line, "Dim" } }, ctx.width, { first = first }))
  end
  out[#out + 1] = { { "  " .. note, "StatusLineDim" } }
  return out
end

-- ---- tools ----------------------------------------------------------------
-- Laid out like the first bone: a label row ("shell cmd", "read_file path
-- (lines 1-20, 20 read)"), shell output and errors in a │ ╰ gutter, edits as
-- a numbered diff. bone.o.tool_detail picks how much: "summary" folds each
-- run of calls into one line ("Read 3 files, ran 2 shell commands"), "rows"
-- is one call per row and "full" shows everything. ctrl+t steps through.

local HINT = " (ctrl+t)"
local LABEL_MAX = 10 -- label rows before "⋮ +N lines"
local GUTTER_MAX = 5 -- gutter rows before the first 2, "⋮ +N", last 2
local PREVIEW_MAX = 5 -- output lines of other tools

local function detail()
  local d = bone.o.tool_detail
  return (d == "rows" or d == "full") and d or "summary"
end

local function expanded()
  return detail() == "full"
end

local function running_mark(item)
  bone.chat.refresh_in(300)
  local now = bone.now()
  local start = item.started_at or now
  if now - start > 86400000 then start = now end
  return math.floor((now - start) / 300) % 2 == 0 and "◌" or "·"
end

local function plural(n, noun)
  return n .. " " .. noun .. (n == 1 and "" or "s")
end

local function more(n, noun)
  return "⋮ +" .. plural(n, noun or "line") .. (expanded() and "" or HINT)
end

local function trimmed_lines(text)
  local ls = lines(text)
  while #ls > 0 and trim(ls[#ls]) == "" do
    ls[#ls] = nil
  end
  while #ls > 0 and trim(ls[1]) == "" do
    table.remove(ls, 1)
  end
  return ls
end

--- Up to `max` rows of text: all of it if it fits, else the first row, a
--- "⋮ +N lines" marker and the last rows.
function bone.ui.preview(text, max, group)
  local ls = trimmed_lines(text)
  max = math.max(max, 1)
  local row = function(l)
    return { { l, group } }
  end
  local out = {}
  if #ls <= max then
    for _, l in ipairs(ls) do
      out[#out + 1] = row(l)
    end
  elseif max < 3 then
    for i = 1, max - 1 do
      out[#out + 1] = row(ls[i])
    end
    out[#out + 1] = { { more(#ls - (max - 1)), "ToolSummary" } }
  else
    local tail = max - 2
    out[1] = row(ls[1])
    out[2] = { { more(#ls - 1 - tail), "ToolSummary" } }
    for i = #ls - tail + 1, #ls do
      out[#out + 1] = row(ls[i])
    end
  end
  return out
end

-- The shell tool ends its output with "[exit code: N]".
local function split_exit_code(text)
  local t = (text or ""):gsub("%s+$", "")
  local body, last = t:match("^(.*)\n([^\n]*)$")
  if not body then
    body, last = "", t
  end
  local code = last:match("^%[exit code: (%-?%d+)%]$")
  if not code then
    body = text or ""
  end
  if trim(body) == "(no output)" then
    body = ""
  end
  return body, tonumber(code)
end

-- read_file rows are "LINE#HASH|text".
local function anchored(l)
  local n, text = l:match("^(%d+)#%w%w|(.*)$")
  return tonumber(n), text
end

local function read_summary(text)
  local first, last, n = nil, nil, 0
  for _, l in ipairs(lines(text)) do
    local num = anchored(l)
    if num then
      first = first or num
      last = num
      n = n + 1
    end
  end
  if n == 0 then
    return "0 lines"
  end
  return "lines " .. first .. "-" .. last .. ", " .. n .. " read"
end

--- Output rows in the shell gutter: "│ " before each, "╰ " before the last.
--- Collapsed to the first and last two rows unless tools are expanded.
local function gutter(text, width, group, gutter_group)
  local rows = {}
  for _, l in ipairs(trimmed_lines(text)) do
    for _, r in ipairs(wrap({ { l, group } }, math.max(width - 8, 1))) do
      rows[#rows + 1] = r
    end
  end
  if not expanded() and #rows > GUTTER_MAX then
    local hidden = #rows - 4
    rows = { rows[1], rows[2], { { more(hidden, "terminal line"), group } }, rows[#rows - 1], rows[#rows] }
  end
  local out = {}
  for i, r in ipairs(rows) do
    local row = { { i == #rows and "      ╰ " or "      │ ", gutter_group } }
    append(row, r)
    out[#out + 1] = row
  end
  return out
end

--- Other tools' output, flush left: the first lines, then "⋮ +N more lines".
local function plain(text, width)
  local ls = trimmed_lines(text)
  local shown = expanded() and #ls or math.min(#ls, PREVIEW_MAX)
  local out = {}
  for i = 1, shown do
    append(out, wrap({ { ls[i], "ToolOutput" } }, width))
  end
  if shown < #ls then
    out[#out + 1] = { { "⋮ +" .. (#ls - shown) .. " more " .. (#ls - shown == 1 and "line" or "lines") .. (expanded() and "" or HINT), "ToolOutput" } }
  end
  return out
end

--- Numbered rows ("   12   text") for expanded file contents.
local function numbered(text, width)
  local out = {}
  for _, l in ipairs(lines(text)) do
    local n, body = anchored(l)
    if n then
      append(out, wrap({ { body, "ToolOutput" } }, width, {
        first = { { "  " .. string.format("%5d", n) .. "   ", "ToolGutter" } },
        rest = { { string.rep(" ", 10), "ToolGutter" } },
      }))
    end
  end
  return out
end

-- Only the lines that changed: drop lines shared at the start and end.
local function diff_lines(old, new)
  local a, b = lines(old), lines(new)
  local s = 0
  while s < #a and s < #b and a[s + 1] == b[s + 1] do
    s = s + 1
  end
  local e = 0
  while e < #a - s and e < #b - s and a[#a - e] == b[#b - e] do
    e = e + 1
  end
  local rows = {}
  for i = s + 1, #a - e do
    rows[#rows + 1] = { sign = "-", text = a[i] }
  end
  for i = s + 1, #b - e do
    rows[#rows + 1] = { sign = "+", text = b[i] }
  end
  return rows
end

--- An anchored edit's result as diff rows with line numbers: context
--- "LINE#HASH|text", "+LINE#HASH|text", "-text" and "..." between hunks.
local function anchored_diff(out)
  local rows, delta, old_n = {}, 0, 0
  for _, l in ipairs(lines(out)) do
    local n, text = anchored(l)
    if n then
      old_n = n - delta
      rows[#rows + 1] = { n = old_n, sign = " ", text = text }
    elseif l:sub(1, 1) == "+" then
      n, text = anchored(l:sub(2))
      if n then
        delta = delta + 1
        rows[#rows + 1] = { n = n, sign = "+", text = text }
      end
    elseif l:sub(1, 1) == "-" then
      old_n = old_n + 1
      delta = delta - 1
      rows[#rows + 1] = { n = old_n, sign = "-", text = l:sub(2) }
    elseif l == "..." then
      rows[#rows + 1] = { separator = true }
    end
  end
  return rows
end

--- Diff rows as the first bone drew them: "   12 - text" on a full-width band.
local function diff_view(rows, width)
  local out = {}
  for _, r in ipairs(rows) do
    if r.separator then
      out[#out + 1] = { { "    ...", "ToolSummary" } }
    else
      local num = r.n and string.format("%5d", r.n) or "     "
      local group = r.sign == "+" and "DiffAdd" or r.sign == "-" and "DiffDelete" or "ToolOutput"
      local band = r.sign ~= " " and group or nil
      local indent = r.text:match("^%s*")
      append(out, wrap({ { r.text:sub(#indent + 1), group } }, width, {
        first = { { "  " .. num .. " " .. r.sign .. " " .. indent, group } },
        rest = { { string.rep(" ", 10) .. indent, group } },
        pad = band,
      }))
    end
  end
  return out
end

local function count(rows, sign)
  local n = 0
  for _, r in ipairs(rows) do
    if r.sign == sign then
      n = n + 1
    end
  end
  return n
end

local function file_label(name, path, summary)
  local t = { { name, "ToolName" }, { " " .. (path or ""), "ToolPath" } }
  if summary then
    t[#t + 1] = { " (" .. summary .. ")", "ToolArgs" }
  end
  return { t }
end

--- "shell" then the command, one label row per command line.
local function shell_label(args)
  local action = args.action or "run"
  if action ~= "run" then
    return { { { "shell", "ToolName" }, { " " .. action .. (args.id and " " .. args.id or ""), "ToolArgs" } } }
  end
  local out = {}
  for _, l in ipairs(lines(args.command or "")) do
    if trim(l) ~= "" then
      local row = { { #out == 0 and "shell " or " ", "ToolName" } }
      append(row, bone.text.shell(l))
      out[#out + 1] = row
    end
  end
  return #out > 0 and out or { { { "shell", "ToolName" } } }
end

--- The built-in content for a tool call: { title = spans or { spans, ... },
--- lines = { spans, ... }, body = rows drawn as they are (gutter, diff) }.
function bone.ui.tool_content(item, ctx)
  local args = type(item.arguments) == "table" and item.arguments or {}
  local out = item.output or ""
  local done, ok = item.done, item.done and not item.is_error
  local width = ctx.width
  local name = item.name
  local err = done and item.is_error and gutter(out, width, "ToolOutput", "ToolError") or nil

  if name == "shell" then
    local body, code = split_exit_code(out)
    local failed = item.is_error or (code ~= nil and code ~= 0)
    if not done then
      -- What it has printed so far (tool/output).
      return { title = shell_label(args), body = gutter(item.live or "", width, "ToolOutput", "ToolGutter") }
    end
    return {
      title = shell_label(args),
      body = gutter(body, width, "ToolOutput", failed and "ToolError" or "ToolGutter"),
    }
  elseif name == "read_file" then
    local summary = ok and (trim(out) == "(empty file)" and "0 lines" or read_summary(out)) or nil
    return {
      title = file_label(name, args.path, summary),
      body = err or (ok and expanded() and numbered(out, width)) or {},
    }
  elseif name == "write_file" then
    local body = {}
    if ok and expanded() then
      for _, l in ipairs(lines(args.content or "")) do
        append(body, wrap({ { l, "ToolOutput" } }, width, { first = { { "      ", "Normal" } }, rest = { { "      ", "Normal" } } }))
      end
    end
    return { title = file_label(name, args.path), body = err or body }
  elseif name == "edit_file" then
    if not ok then
      return { title = file_label(name, args.path), body = err or {} }
    end
    local rows = args.old_string and diff_lines(args.old_string, args.new_string or "") or anchored_diff(out)
    local summary = "-" .. count(rows, "-") .. " | +" .. count(rows, "+")
    local times = tonumber(out:match("^Replaced (%d+) "))
    if times and times > 1 then
      summary = summary .. " ×" .. times
    end
    return { title = file_label(name, args.path, summary), body = diff_view(rows, width) }
  end

  -- A subagent call reads as its agent and task: "reviewer check the diff".
  local target = args.path or args.query or args.task
  local agent = name == "subagent" and args.name ~= "" and args.name
  local title = { { type(agent) == "string" and agent or name, "ToolName" } }
  if type(target) == "string" and target ~= "" then
    title[#title + 1] = { " " .. target, "ToolArgs" }
  end
  return { title = { title }, body = err or (done and plain(out, width)) or {} }
end

local function as_spans(v, group)
  if type(v) == "string" then
    return { { v, group } }
  end
  return v or {}
end

-- A title is one line of spans or a list of them.
local function title_lines(title)
  if type(title) == "table" and type(title[1]) == "table" and type(title[1][1]) == "table" then
    return title
  end
  return { as_spans(title, "ToolName") }
end

-- ---- tool summaries ------------------------------------------------------

-- Folded into a summary line: everything but edits and sub-agents.
local function foldable(t)
  return t.name ~= "edit_file" and t.name ~= "subagent"
end

local function arg(t, key)
  local args = type(t.arguments) == "table" and t.arguments or {}
  local v = args[key]
  return type(v) == "string" and v or nil
end

-- What a run of calls did, in the order first seen: "read 3 files",
-- "ran 2 shell commands", "wrote 1 file", "called web_search 2 times".
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
    if t.name == "read_file" then
      bump("read", function(n) return "read " .. plural(n, "file") end, arg(t, "path") or t.id)
    elseif t.name == "write_file" then
      bump("write", function(n) return "wrote " .. plural(n, "file") end, arg(t, "path") or t.id)
    elseif t.name == "shell" then
      bump("shell", function(n) return "ran " .. plural(n, "shell command") end)
    else
      bump("tool:" .. t.name, function(n) return "called " .. t.name .. (n > 1 and " " .. n .. " times" or "") end)
    end
  end
  local parts = {}
  for _, s in ipairs(order) do
    parts[#parts + 1] = s.phrase(s.n)
  end
  local text = table.concat(parts, ", ")
  local failed = 0
  for _, t in ipairs(calls) do
    failed = failed + (t.done and t.is_error and 1 or 0)
  end
  text = text:sub(1, 1):upper() .. text:sub(2)
  return failed > 0 and (text .. ", " .. failed .. " failed") or text
end

--- In summary mode: the line for the first foldable call of a stretch (the
--- calls between edits and failures, reasoning aside), nil for the rest.
--- false when this call is not folded.
local function summary_view(item, ctx)
  if not foldable(item) then
    return false
  end
  -- Not the first of its stretch: the call before it (reasoning aside)
  -- folds too. Only the first reads the stretch, so a long one costs one
  -- read rather than one per call.
  local i = item.index - 1
  local before = bone.chat.item(i)
  while before and before.kind == "reasoning" do
    i = i - 1
    before = bone.chat.item(i)
  end
  if before and before.kind == "tool" and foldable(before) then
    return nil
  end
  -- This call and the ones after it (reasoning between them does not
  -- break the stretch; an edit or sub-agent does).
  local stretch = {}
  for _, t in ipairs(bone.chat.items({ around = item.index, from = item.index, kind = { "tool", "reasoning" } })) do
    if t.kind == "tool" then
      if not foldable(t) then
        break
      end
      stretch[#stretch + 1] = t
    end
  end
  local running = false
  for _, t in ipairs(stretch) do
    running = running or not t.done
  end
  local out = starts("tool", ctx)
  append(out, wrap({ { summary(stretch), "ToolArgs" } }, ctx.width, {
    first = running and { { "  " .. running_mark(item) .. " ", "ToolRunning" } } or { { "    ", "Normal" } },
    rest = { { "    ", "Normal" } },
  }))
  return out
end

--- The label rows (marker on the first), then the output.
function views.tool(item, ctx)
  if detail() == "summary" then
    local folded = summary_view(item, ctx)
    if folded ~= false then
      return folded
    end
  end
  local custom = bone.ui.tool_views[item.name]
  local content = custom and custom(item, ctx) or bone.ui.tool_content(item, ctx)

  local marker
  if not item.done then
    marker = { "  " .. running_mark(item) .. " ", "ToolRunning" }
  elseif item.is_error then
    marker = { "  ✕ ", "ToolError" }
  else
    marker = { "    ", "Normal" }
  end
  local out = starts("tool", ctx)
  local label = {}
  for i, t in ipairs(title_lines(content.title)) do
    append(label, wrap(t, ctx.width, {
      first = { i == 1 and marker or { "    ", "Normal" } },
      rest = { { "    ", "Normal" } },
    }))
  end
  if not expanded() and #label > LABEL_MAX then
    local hidden = #label - (LABEL_MAX - 1)
    for i = #label, LABEL_MAX, -1 do
      label[i] = nil
    end
    label[#label + 1] = { { "    " .. more(hidden, item.name == "shell" and "command line" or "line"), "ToolArgs" } }
  end
  append(out, label)

  if content.body then
    append(out, content.body)
  end
  -- Custom views' lines sit under the label; errors always show.
  local body = {}
  for _, l in ipairs(content.lines or {}) do
    local row = { { "    ", "Normal" } }
    append(row, bone.text.clip(as_spans(l, "ToolOutput"), ctx.width - 4))
    body[#body + 1] = row
  end
  if custom and item.is_error and #body == 0 then
    body = gutter(item.output or "", ctx.width, "ToolOutput", "ToolError")
  end
  append(out, body)
  return out
end
