-- /help: searchable commands, keyboard shortcuts and embedded documentation.
-- Commands come from the live registry, including user and plugin commands.
-- Enter prepares a command in the prompt, or opens documentation above this
-- browser; closing the document returns to the same search and selection.

local M = {}
local CHAR = "^[%z\1-\127\194-\244][\128-\191]*$"

-- Help browser and topic lookup. Override ~/.bone/runtime/lua/bone/help.lua
-- to customize it, or replace bone.ui.help from tui.lua.
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

local function open_section(secs, best)
  local s, lines = secs[best], {}
  for i = best, #secs do
    if i > best and (secs[i].doc ~= s.doc or secs[i].level <= s.level) then break end
    for _, l in ipairs(secs[i].lines) do lines[#lines + 1] = l end
  end
  return bone.ui.pager(table.concat(lines, "\n"), { title = s.doc .. ".md: " .. s.title })
end

function M.topic(topic)
  local t = topic:lower():match("^%s*(.-)%s*$")
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
  return open_section(secs, best)
end

-- The default shortcuts, grouped by what you are doing. Keep these in sync
-- with tui/defaults.lua; user mappings can replace the defaults.
local KEYS = {
  { "enter", "send a message; during a turn, queue it", "Messaging" },
  { "ctrl+v / super+v", "paste clipboard screenshot or text (/paste if intercepted)", "Messaging" },
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
  { "esc in a sub-agent", "return to the main session when the prompt is empty", "Tray" },
  { "ctrl+left / ctrl+right", "move by word (also alt+left / alt+right)", "Editing" },
  { "home / end", "start or end of line (also ctrl+a / ctrl+e)", "Editing" },
  { "ctrl+w / alt+backspace", "delete the word before the cursor", "Editing" },
  { "ctrl+u / ctrl+k", "delete to the start or end of the line", "Editing" },
  { "ctrl+p", "move focus to the next panel", "Panels" },
  { "tab / shift+tab in a panel", "move to the next or previous panel", "Panels" },
  { "esc in a panel", "return focus to the prompt", "Panels" },
}

function M.open()
  local st = { tab = 1, query = "", sel = 1, rows = 1 }
  local tabs = { "Commands", "Keys", "Docs" }
  local docs = {}
  local sections = doc_sections()
  for _, d in ipairs(bone._docs) do
    docs[#docs + 1] = { label = d.name .. ".md", desc = "Read the complete " .. d.name .. " guide", topic = d.name, meta = "Full guide" }
  end
  for i, s in ipairs(sections) do
    docs[#docs + 1] = { label = s.title:gsub("`", ""), desc = "Read this section of " .. s.doc .. ".md", meta = s.doc .. ".md", section = i }
  end

  local function matches()
    local all = {}
    if st.tab == 1 then
      for _, c in ipairs(bone.cmd.list()) do
        local aliases = #c.aliases > 0 and ("aliases: " .. table.concat(c.aliases, ", ")) or "Slash command"
        all[#all + 1] = { label = "/" .. c.name, desc = c.desc or "", meta = aliases, command = c.name }
      end
    elseif st.tab == 2 then
      for _, k in ipairs(KEYS) do
        all[#all + 1] = { label = k[1], desc = k[2], meta = k[3] .. " · default shortcut", topic = "keys" }
      end
    else
      all = docs
    end
    local out = {}
    for _, it in ipairs(all) do
      local text = (it.label .. " " .. it.desc .. " " .. it.meta):lower()
      local match = true
      for word in st.query:lower():gmatch("%S+") do
        if not text:find(word, 1, true) then
          match = false
          break
        end
      end
      if match then out[#out + 1] = it end
    end
    return out
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
      clip({ { "Search  ", "Accent" }, { st.query == "" and "type to filter…" or st.query, st.query == "" and "Dim" or "Normal" }, { " ▏", "Accent" } }, inner),
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
      if st.tab == 2 then meta = meta .. " · enter opens the keys guide" end
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
      local it = list[st.sel]
      if it and it.command then
        bone.ui.close(id)
        bone.prompt.set("/" .. it.command .. " ")
      elseif it and it.section then
        -- Use this exact section, rather than searching a duplicate heading.
        open_section(sections, it.section)
      elseif it then
        M.topic(it.topic)
      end
    elseif k == "backspace" then
      st.query, st.sel = st.query:gsub("[%z\1-\127\194-\244][\128-\191]*$", ""), 1
    elseif k == "ctrl+u" then
      st.query, st.sel = "", 1
    elseif k == "space" then
      st.query, st.sel = st.query .. " ", 1
    elseif k:match(CHAR) then
      st.query, st.sel = st.query .. k, 1
    else
      return false
    end
    return true
  end
  id = bone.ui.popup({ lines = render, on_key = on_key })
  return id
end

-- /md shares this already-embedded module: no Rust manifest change needed.
local markdown_close
function M.markdown(path)
  path = (path or ""):match("^%s*(.-)%s*$"):gsub("^%./", "")
  if path:sub(1, 1) == "/" or path:find("%z") or path:find("^%.%./")
    or path:find("/%.%./") or path:match("/%.%.$") or path == ".." then
    return bone.notify("/md expects a relative Markdown path inside the working directory", "error")
  end
  if markdown_close then markdown_close() end
  local session = bone.chat.session() or bone.api.session() or {}
  local cwd = session.cwd or "."
  local st = { files = {}, filtered = {}, query = "", sel = 1, pane = "files",
    rows = 1, top = 0, total = 0, positions = {}, loading = true }
  local id, scan, read, closed, close_event
  local scan_version, read_version = 0, 0
  local MAX_BYTES, MAX_FILES = 1024 * 1024, 20000
  local function alive() return not closed and bone.ui.is_open(id) end
  local function cancel(job) if job then job:cancel() end end
  local function close()
    if closed then return end
    closed = true
    cancel(scan)
    cancel(read)
    if close_event then bone.off(close_event) end
    bone.ui.close(id)
    markdown_close = nil
  end
  local function select_file()
    local file = st.filtered[st.sel]
    if file == st.file then return end
    if st.file then st.positions[st.file] = st.top end
    st.file, st.top = file, file and st.positions[file] or 0
    st.text, st.lines, st.read_error = nil, nil, nil
    read_version = read_version + 1
    local version = read_version
    cancel(read)
    read = nil
    if not file then return end
    local chunks, bytes, stderr = {}, 0, ""
    -- argv, not shell interpolation: spaces, quotes and newlines are safe.
    read = bone.job.start({ "head", "-c", tostring(MAX_BYTES + 1), "--", "./" .. file }, {
      name = "md read", cwd = cwd, timeout = 10000,
      on_stdout = function(data)
        if not alive() or version ~= read_version then return end
        local piece = data:sub(1, math.max(0, MAX_BYTES + 1 - bytes))
        chunks[#chunks + 1], bytes = piece, bytes + #piece
      end,
      on_stderr = function(data) stderr = (stderr .. data):sub(1, 2048) end,
      on_exit = function(r)
        if not alive() or version ~= read_version then return end
        read = nil
        if r.state ~= "exited" or r.code ~= 0 then
          st.read_error = "Cannot read file: " .. (stderr ~= "" and stderr or r.error or r.state)
        elseif bytes > MAX_BYTES then st.read_error = "File exceeds the 1 MiB reader limit."
        else
          st.text = table.concat(chunks):gsub("\r\n", "\n")
          if st.text:find("%z") then st.text, st.read_error = nil, "Not a text Markdown file (contains NUL bytes)." end
        end
      end,
    })
  end
  local function filter()
    st.filtered = {}
    for _, file in ipairs(st.files) do
      local match = true
      for word in st.query:lower():gmatch("%S+") do
        if not file:lower():find(word, 1, true) then match = false; break end
      end
      if match then st.filtered[#st.filtered + 1] = file end
    end
    st.sel = math.max(1, math.min(st.sel, #st.filtered))
    select_file()
  end
  local function discover(wanted)
    scan_version = scan_version + 1
    local version = scan_version
    cancel(scan)
    st.loading, st.scan_error = true, nil
    local files, pending, stderr, limited = {}, "", "", false
    scan = bone.job.start({ "find", "-P", ".", "-name", ".git", "-prune", "-o",
      "-type", "f", "-iname", "*.md", "-print0" }, {
      name = "md scan", cwd = cwd, timeout = 30000,
      on_stdout = function(data, job)
        if not alive() or version ~= scan_version or limited then return end
        pending = pending .. data
        local start = 1
        while true do
          local stop = pending:find("\0", start, true)
          if not stop then break end
          files[#files + 1] = pending:sub(start, stop - 1):gsub("^%./", "")
          start = stop + 1
          if #files >= MAX_FILES then limited = true; job:cancel(); break end
        end
        pending = pending:sub(start)
      end,
      on_stderr = function(data) stderr = (stderr .. data):sub(1, 2048) end,
      on_exit = function(r)
        if not alive() or version ~= scan_version then return end
        scan, st.loading = nil, false
        table.sort(files)
        st.files, st.sel = files, 1
        if limited then st.scan_error = "Showing the first 20,000 files; scan limit reached."
        elseif r.state ~= "exited" or r.code ~= 0 then
          st.scan_error = "Scan incomplete: " .. (stderr ~= "" and stderr or r.error or r.state)
        end
        filter()
        if wanted and wanted ~= "" then
          local found = false
          for i, file in ipairs(st.filtered) do
            if file == wanted then st.sel, found = i, true; break end
          end
          if found then select_file(); st.pane = "reader"
          else bone.notify("Markdown file not found: " .. wanted, "error") end
        end
      end,
    })
  end
  local function clip(spans, width, pad)
    local out, left = {}, width
    for _, span in ipairs(spans) do
      if left <= 0 then break end
      local text = bone.text.truncate(span[1]:gsub("[%z\1-\31\127]", " "), left)
      out[#out + 1], left = { text, span[2] }, left - bone.text.width(text)
    end
    if pad and left > 0 then out[#out + 1] = { string.rep(" ", left), "Normal" } end
    return out
  end
  local function render(ctx)
    local w, h = math.max(1, ctx.width - 4), math.max(1, ctx.height - 2)
    if w < 20 or h < 8 then
      return { clip({ { "Markdown · enlarge terminal · esc close", "Dim" } }, ctx.width) }
    end
    local inner = w - 4
    local split = inner >= 76
    local left = split and math.min(32, math.floor(inner * 0.3)) or inner
    local right = split and (inner - left - 3) or inner
    st.rows = h - 6
    local reader = {}
    if st.file and st.text then
      local md_width = math.min(right, 88)
      if not st.lines or st.line_width ~= md_width then
        st.lines, st.line_width = bone.ui.markdown(st.text, md_width, ""), md_width
      end
      reader = st.lines
      if #reader == 0 then reader = { { { "Empty Markdown file.", "Dim" } } } end
    else
      local message = st.read_error or (st.file and "Loading document…" or "Select a Markdown file to read.")
      reader = bone.text.wrap({ { message, st.read_error and "Error" or "Dim" } }, right)
    end
    st.total = #reader
    if st.text then st.top = math.max(0, math.min(st.top, st.total - st.rows)) end
    local visible_top = st.text and st.top or 0
    local first, list = math.max(1, st.sel - st.rows + 1), {}
    for i = first, math.min(#st.filtered, first + st.rows - 1) do
      list[#list + 1] = { { (i == st.sel and "› " or "  ") .. st.filtered[i], i == st.sel and "Selection" or "Normal" } }
    end
    if #list == 0 then
      list = { { { st.loading and "Scanning…" or (#st.files == 0 and "No Markdown files." or "No matches."), "Dim" } } }
    end
    local function join(a, b)
      if not split then return clip(st.pane == "files" and a or b, inner) end
      local out = clip(a, left, true)
      out[#out + 1] = { " │ ", "PopupBorder" }
      for _, span in ipairs(clip(b, right)) do out[#out + 1] = span end
      return out
    end
    local body = {
      join({ { "Files · " .. #st.filtered .. (st.pane == "files" and " ◂" or ""), "Accent" } },
        { { (st.file or "Preview") .. (st.pane == "reader" and " ◂" or ""), "Accent" } }),
      clip({ { st.loading and "Scanning Markdown files…" or "Filter: " .. (st.query == "" and "type in file pane…" or st.query), "Dim" } }, inner),
      { { string.rep("─", inner), "PopupBorder" } },
    }
    for i = 1, st.rows do body[#body + 1] = join(list[i] or {}, reader[visible_top + i] or {}) end
    local position = st.file and string.format("%d/%d · ", math.min(st.top + st.rows, st.total), st.total) or ""
    body[#body + 1] = clip({ { st.scan_error or (position .. "↑↓ move/scroll · enter read · tab panes · ctrl+r refresh · esc close"), "Dim" } }, inner)
    return bone.ui.box(body, { title = "Markdown · " .. cwd, width = w })
  end
  local function on_key(k)
    if k == "esc" then close()
    elseif k == "tab" or k == "shift+tab" or k == "backtab" then
      st.pane = st.pane == "files" and "reader" or "files"
    elseif k == "enter" then st.pane = "reader"
    elseif k == "ctrl+r" then
      local wanted = st.file
      if wanted then st.positions[wanted] = st.top end
      st.file = nil
      read_version = read_version + 1
      cancel(read)
      st.text, st.lines = nil, nil
      discover(wanted)
    elseif k == "up" or k == "down" or k == "wheelup" or k == "wheeldown"
      or k == "pageup" or k == "pagedown" or k == "home" or k == "end"
      or (st.pane == "reader" and k == "space") then
      local by = (k == "up" or k == "wheelup" or k == "pageup") and -1 or 1
      if k == "pageup" or k == "pagedown" or k == "space" then by = by * st.rows end
      if st.pane == "files" then
        st.sel = k == "home" and 1 or k == "end" and #st.filtered or st.sel + by
        st.sel = math.max(1, math.min(st.sel, #st.filtered))
        select_file()
      else
        st.top = k == "home" and 0 or k == "end" and st.total or st.top + by
        st.top = math.max(0, math.min(st.top, st.total - st.rows))
      end
    elseif st.pane == "files" then
      if k == "backspace" then st.query = st.query:gsub(CHAR:sub(2, -2) .. "$", "")
      elseif k == "ctrl+u" then st.query = ""
      elseif k == "space" then st.query = st.query .. " "
      elseif k:match(CHAR) then st.query = st.query .. k
      else return false end
      st.sel = 1
      filter()
    else return false end
    return true
  end
  id = bone.ui.popup({ lines = render, on_key = on_key })
  markdown_close = close
  close_event = bone.on("panel/closed", function(ev) if ev.id == id and ev.kind == "popup" then close() end end)
  discover(path)
  return id
end
return M
