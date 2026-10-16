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

-- bone.ui.markdown (Markdown text to lines) is in the base API, tui/api.lua.

-- A streaming item is drawn in two parts: a settled start, drawn once and
-- kept, and the rest, drawn every frame, so a long reply doesn't cost more
-- with every token. `cut(text, from)` is where the settled part may end,
-- since nothing after it changes how the text before it draws.
local kept = {}

local function streamed(item, text, width, cut, draw)
  if not item.streaming then
    kept = {}
    return draw(text)
  end
  local id = item.kind .. item.key
  local k = kept[id]
  if not (k and k.width == width and text:sub(1, #k.text) == k.text) then
    k = { text = "", width = width, lines = {} }
    kept[id] = k
  end
  local at = cut(text, #k.text + 1)
  if at then
    append(k.lines, draw(text:sub(#k.text + 1, at - 1)))
    k.text = text:sub(1, at - 1)
  end
  return append(append({}, k.lines), draw(text:sub(#k.text + 1)))
end

-- After the last line break.
local function after_line(text, from)
  return text:match("^.*\n()", from)
end

-- Before the last line that follows a blank one outside a code block:
-- Markdown blocks never run past such a line (see markdown.rs).
local function after_blank(text, from)
  local at, blank, fence = nil, false, false
  for line in text:sub(from):gmatch("([^\n]*)\n") do
    local empty = line:find("^%s*$")
    if blank and not empty then
      at = from
    end
    blank = empty and not fence
    if line:find("^%s*```") or line:find("^%s*~~~") then
      fence = not fence
    end
    from = from + #line + 1
  end
  return at
end

-- ---- messages -------------------------------------------------------------

function views.user(item, ctx)
  local out = { {} } -- A blank line above every user message, including the first.
  local band = "UserMessage"
  for i, line in ipairs(lines(item.text)) do
    local first = i == 1 and { { "› ", "UserPrompt" } } or { { "  ", band } }
    append(out, wrap({ { line, band } }, ctx.width, { first = first, rest = { { "  ", band } }, pad = band }))
  end
  for _, image in ipairs(item.images or {}) do
    append(out, wrap({ { ("  [Image: %s · %d×%d]"):format(image.name, image.width, image.height), "Dim" } }, ctx.width))
  end
  return out
end

function views.reasoning(item, ctx)
  local text = trim(item.text)
  if not bone.o.show_reasoning or text == "" then
    return nil
  end
  local prefix = item.streaming and "∴ " or "│ "
  return append(starts("reasoning", ctx), streamed(item, text, ctx.width, after_line, function(part)
    local out = {}
    for _, line in ipairs(lines(part)) do
      append(out, wrap({ { line, "Reasoning" } }, ctx.width,
        { first = { { prefix, "Reasoning" } }, rest = { { "  ", "Reasoning" } } }))
    end
    return out
  end))
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
  local md = streamed(item, text, ctx.width, after_blank, function(part)
    local md = bone.ui.markdown(part, ctx.width, "  ")
    -- A settled part ends in a blank line, which is left out at the end.
    if part:sub(-1) == "\n" then
      md[#md + 1] = {}
    end
    return md
  end)
  if item.streaming then
    if #md > 0 then
      md[#md] = append({}, md[#md])
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
  for _, image in ipairs(item.images or {}) do
    append(out, wrap({ { ("  [Image: %s · %d×%d]"):format(image.name, image.width, image.height), "Dim" } }, ctx.width))
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

local HINT = " (click or ctrl+t)"
local toggled = {}
local last_detail

-- Item keys are stable within a chat, but can repeat in another session.
local function tool_key(item)
  local s = bone.chat.session()
  return (s and s.session_id or "") .. ":" .. item.key
end

local LABEL_MAX = 10 -- label rows before "⋮ +N lines"
local GUTTER_MAX = 5 -- gutter rows before the first 2, "⋮ +N", last 2
local PREVIEW_MAX = 5 -- output lines of other tools
local AGENT_PREVIEW_MAX = 3 -- rendered report rows under a sub-agent's task

local function detail()
  local d = bone.o.tool_detail
  d = (d == "rows" or d == "full") and d or "summary"
  if d ~= last_detail then
    toggled = {}
    last_detail = d
  end
  return d
end

local function expanded(ctx)
  if ctx and ctx.tool_expanded ~= nil then return ctx.tool_expanded end
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

local function more(n, noun, ctx)
  return "⋮ +" .. plural(n, noun or "line") .. (expanded(ctx) and "" or HINT)
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

-- Stored legacy read_file rows are "LINE#HASH|text".
local function anchored(l)
  local n, text = l:match("^(%d+)#%w%w|(.*)$")
  return tonumber(n), text
end

-- Recognize an entire anchored transcript, not individual anchor-looking lines
-- in a plain file. Notices are metadata, not file lines (including their spacer).
local function read_rows(text, args)
  if text == "(empty file)" then
    return { { text = text, notice = true } }
  end
  local ls, notice = lines(text), nil
  if #ls > 1 and ls[#ls - 1] == "" and
      ls[#ls]:match("^%[showing lines %d+%-%d+ of %d+; use offset to read more%]$") then
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
    if r.n then
      first, last, n = first or r.n, r.n, n + 1
    end
  end
  if n == 0 then return "0 lines" end
  return "lines " .. first .. "-" .. last .. ", " .. n .. " read"
end

--- Output rows in the shell gutter: "│ " before each, "╰ " before the last.
--- Collapsed to the first and last two rows unless tools are expanded.
local function gutter(text, width, group, gutter_group, ctx)
  local rows = {}
  for _, l in ipairs(trimmed_lines(text)) do
    for _, r in ipairs(wrap({ { l, group } }, math.max(width - 8, 1))) do
      rows[#rows + 1] = r
    end
  end
  if not expanded(ctx) and #rows > GUTTER_MAX then
    local hidden = #rows - 4
    rows = { rows[1], rows[2], { { more(hidden, "terminal line", ctx), group } }, rows[#rows - 1], rows[#rows] }
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
local function plain(text, width, ctx)
  local ls = trimmed_lines(text)
  local shown = expanded(ctx) and #ls or math.min(#ls, PREVIEW_MAX)
  local out = {}
  for i = 1, shown do
    append(out, wrap({ { ls[i], "ToolOutput" } }, width))
  end
  if shown < #ls then
    out[#out + 1] = { { "⋮ +" .. (#ls - shown) .. " more " .. (#ls - shown == 1 and "line" or "lines") .. (expanded(ctx) and "" or HINT), "ToolOutput" } }
  end
  return out
end

--- Numbered rows ("   12   text") for expanded file contents.
local function numbered(rows, width)
  local out = {}
  for _, r in ipairs(rows) do
    local prefix = r.n and ("  " .. string.format("%5d", r.n) .. "   ") or "          "
    append(out, wrap({ { r.text, r.notice and "ToolSummary" or "ToolOutput" } }, width, {
      first = { { prefix, "ToolGutter" } },
      rest = { { string.rep(" ", 10), "ToolGutter" } },
    }))
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

-- A delegated task is a small report, with its own hierarchy and Markdown.
-- Keep the preview inside the same gutter as the expanded report so it
-- cannot be mistaken for the main assistant's answer.
local function agent_content(item, ctx, args)
  local name = type(args.name) == "string" and trim(args.name) or ""
  if name == "" then name = "subagent" end
  local task = type(args.task) == "string" and trim(args.task):gsub("%s+", " ") or ""
  local title = { { task ~= "" and task or name, "Accent" } }
  if task ~= "" then
    title[#title + 1] = { " · " .. name, "Dim" }
  end
  local status = not item.done and "running" or item.is_error and "failed" or "done"
  title[#title + 1] = { " · " .. status, item.is_error and "ToolError" or "Dim" }
  -- Prefer persisted tool timing: the agent record is only in memory.
  local agent = require("bone.agents").for_call(item.id)
  local start = item.started_at or (agent and agent.started_at)
  local ms = item.duration_ms or (not item.done and start and bone.now() - start)
    or (agent and agent.started_at and agent.finished_at and agent.finished_at - agent.started_at)
  local stats = ms and bone.util.duration(math.max(ms, 0) / 1000)
  if agent and agent.tokens > 0 then
    stats = bone.util.tokens(agent.tokens) .. " tok" .. (stats and (" · " .. stats) or "")
  end
  if stats then title[#title + 1] = { " · " .. stats, "Dim" } end
  if not item.done then bone.chat.refresh_in(1000) end

  local report = bone.ui.markdown(trim(item.output or item.live or ""), math.max(ctx.width - 8, 1))
  local shown = (expanded(ctx) or item.is_error) and #report or math.min(#report, AGENT_PREVIEW_MAX)
  if not expanded(ctx) and not item.is_error then
    -- Stop at the end of the opening paragraph, rather than leaving a
    -- dangling "Changes:" label. A leading heading can keep its paragraph.
    local heading = false
    for _, span in ipairs(report[1] or {}) do heading = heading or span[2] == "MdHeading" end
    for i = 2, shown do
      if #report[i] == 0 and not (heading and i == 2) then
        shown = i - 1
        break
      end
    end
  end
  -- A blank at the preview boundary adds space without saying anything.
  while shown > 0 and #report[shown] == 0 do shown = shown - 1 end
  local body = {}
  for i = 1, shown do body[#body + 1] = report[i] end
  if shown < #report then
    body[#body + 1] = { { more(#report - shown, "report line", ctx), "ToolSummary" } }
  end
  local gutter_group = item.is_error and "ToolError" or "ToolGutter"
  local rows = {}
  for i, line in ipairs(body) do
    local row = { { i == #body and "      ╰ " or "      │ ", gutter_group } }
    append(row, line)
    rows[#rows + 1] = row
  end
  return { title = title, body = rows }
end

--- The built-in content for a tool call: { title = spans or { spans, ... },
--- lines = { spans, ... }, body = rows drawn as they are (gutter, diff) }.
function bone.ui.tool_content(item, ctx)
  local args = type(item.arguments) == "table" and item.arguments or {}
  local out = item.output or ""
  local done, ok = item.done, item.done and not item.is_error
  local width = ctx.width
  local name = item.name
  local err = done and item.is_error and gutter(out, width, "ToolOutput", "ToolError", ctx) or nil

  if name == "subagent" then
    return agent_content(item, ctx, args)
  elseif name == "shell" then
    local body, code = split_exit_code(out)
    local failed = item.is_error or (code ~= nil and code ~= 0)
    if not done then
      -- What it has printed so far (tool/output).
      return { title = shell_label(args), body = gutter(item.live or "", width, "ToolOutput", "ToolGutter", ctx) }
    end
    return {
      title = shell_label(args),
      body = gutter(body, width, "ToolOutput", failed and "ToolError" or "ToolGutter", ctx),
    }
  elseif name == "read_file" then
    local rows = ok and read_rows(out, args) or {}
    local summary = ok and read_summary(rows) or nil
    return {
      title = file_label(name, args.path, summary),
      body = err or (ok and expanded(ctx) and numbered(rows, width)) or {},
    }
  elseif name == "write_file" then
    local body = {}
    if ok and expanded(ctx) then
      for _, l in ipairs(lines(args.content or "")) do
        append(body, wrap({ { l, "ToolOutput" } }, width, { first = { { "      ", "Normal" } }, rest = { { "      ", "Normal" } } }))
      end
    end
    return { title = file_label(name, args.path), body = err or body }
  elseif name == "edit_file" then
    if not ok then
      return { title = file_label(name, args.path), body = err or {} }
    end
    local edits = type(args.edits) == "table" and args.edits or
      (type(args.old_string) == "string" and { args } or nil)
    local rows, all = {}, false
    if edits then
      -- Argument previews, not an actual file diff: offsets and per-edit
      -- replace_all occurrence counts are unknown. Never multiply a batch
      -- preview by the aggregate replacement count in the tool output.
      for i, edit in ipairs(edits) do
        if i > 1 then rows[#rows + 1] = { separator = true } end
        append(rows, diff_lines(edit.old_string or "", edit.new_string or ""))
        all = all or edit.replace_all == true
      end
    else
      rows = anchored_diff(out)
    end
    local summary = (edits and "preview: " or "") .. "-" .. count(rows, "-") .. " | +" .. count(rows, "+")
    if edits and #edits > 1 then summary = summary .. "; " .. plural(#edits, "edit") end
    if all then summary = summary .. "; replace_all" end
    return { title = file_label(name, args.path, summary), body = diff_view(rows, width) }
  end

  local target = args.path or args.query or args.task
  local title = { { name, "ToolName" } }
  if type(target) == "string" and target ~= "" then
    title[#title + 1] = { " " .. target, "ToolArgs" }
  end
  return { title = { title }, body = err or (done and plain(out, width, ctx)) or {} }
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

-- Type live summaries, preserving their common prefix when the text changes.
-- Completed history appears immediately; keys include the session.
local TYPE_MS = 12
local typing = {}

local function chars(s)
  local t = {}
  for c in s:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
    t[#t + 1] = c
  end
  return t
end

local function typed(id, text, running)
  local now = bone.now()
  local st = typing[id] or { shown = running and "" or text, at = now }
  typing[id] = st
  if st.shown ~= text then
    local want, have = chars(text), chars(st.shown)
    local keep = 0
    while keep < #have and keep < #want and have[keep + 1] == want[keep + 1] do
      keep = keep + 1
    end
    local n = math.min(#want, keep + math.max(1, math.floor((now - st.at) / TYPE_MS)))
    st.shown = table.concat(want, "", 1, n)
    if n < #want then bone.chat.refresh_in(TYPE_MS) end
  end
  st.at = now
  return st.shown
end

--- In summary mode: the line for the first foldable call of a stretch (the
--- calls between edits and failures, reasoning aside), nil for the rest.
--- false when this call is not folded.
local render_tool

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
  append(out, wrap({ { typed(tool_key(item), summary(stretch), running), "ToolArgs" } }, ctx.width, {
    first = running and { { "  " .. running_mark(item) .. " ", "ToolRunning" } } or { { "    ", "Normal" } },
    rest = { { "    ", "Normal" } },
  }))
  if toggled[tool_key(item)] then
    for _, t in ipairs(stretch) do
      -- The summary query omits costly output; fetch it only when expanded.
      append(out, render_tool(bone.chat.item(t.index, { full = true }), ctx, true))
    end
  end
  return out
end

--- The label rows (marker on the first), then the output.
render_tool = function(item, ctx, full)
  ctx = { width = ctx.width, region = ctx.region, prev = ctx.prev, tool_expanded = full }
  local custom = bone.ui.tool_views[item.name]
  local content = custom and custom(item, ctx) or bone.ui.tool_content(item, ctx)

  local marker
  if not item.done then
    marker = { "  " .. running_mark(item) .. " ", "ToolRunning" }
  elseif item.is_error then
    marker = { "  ✕ ", "ToolError" }
  elseif item.name == "subagent" then
    marker = { "  ✓ ", "Accent" }
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
  if not expanded(ctx) and #label > LABEL_MAX then
    local hidden = #label - (LABEL_MAX - 1)
    for i = #label, LABEL_MAX, -1 do
      label[i] = nil
    end
    label[#label + 1] = { { "    " .. more(hidden, item.name == "shell" and "command line" or "line", ctx), "ToolArgs" } }
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
    body = gutter(item.output or "", ctx.width, "ToolOutput", "ToolError", ctx)
  end
  append(out, body)
  return out
end

function views.tool(item, ctx)
  local mode = detail()
  if mode == "summary" then
    local folded = summary_view(item, ctx)
    if folded ~= false then return folded end
  end
  local full = mode == "full"
  if toggled[tool_key(item)] then full = not full end
  return render_tool(item, ctx, full)
end

local press
bone.on("mouse", function(ev)
  local down = press
  press = ev.action == "down" and not ev.popup and not ev.panel and ev or nil
  -- Leave selection events alone; only a release without a drag toggles.
  if ev.action ~= "up" or ev.button ~= "left" or not down or down.button ~= "left"
      or down.x ~= ev.x or down.y ~= ev.y or down.index ~= ev.index
      or not ev.index or ev.popup or ev.panel then
    return
  end
  local item = bone.chat.item(ev.index)
  if not item or item.kind ~= "tool" then return end
  -- A live sub-agent's title still opens its session; its report toggles.
  if item.name == "subagent" and (ev.line or 1) <= 2 and require("bone.agents").for_call(item.id) then
    return
  end
  detail() -- reset local overrides after ctrl+t changes the global level
  toggled[tool_key(item)] = not toggled[tool_key(item)]
  bone.chat.redraw(ev.index)
end)
