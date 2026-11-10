-- Terminals (/term, ctrl+]) for the GUI: a shell, or any command, in a
-- pseudo-terminal (bone.job with pty), drawn as a panel's content. It is
-- the term plugin's module, over shared APIs only.
--
-- A shell, or any command, in a pseudo-terminal,
-- drawn as a panel's content. Where it goes is the panel's business: any
-- bone.ui.panel option (dock, size, hidden, …) passes through, or put
-- "panel:<id>" in bone.ui.layout. While it has the keyboard every key goes to
-- the program except those in M.escape, which gives the keyboard back to the prompt;
-- the wheel and shift+page keys scroll back; xterm mouse reporting
-- receives clicks, drags and wheel events. In the desktop app the screen is
-- laid out on a grid of character cells, and when the program does not take
-- the mouse a drag selects (a double click a word, a triple click a line);
-- releasing copies the selection, and ctrl+c while there is one copies it
-- again and ends it (with none, ctrl+c goes to the program).
--
--   local term = require("bone.terminal")
--   term.open({ id = "logs", dock = "right", size = 0.5 }, { cmd = "tail -f log" })

-- Keep the public facade across reloads, including callers holding open.
-- This is deliberately not persisted plugin state.
local M = bone._gui_term_module or {}
if M._active then return M end -- A dependency can evict package.loaded while we run.
bone._gui_term_module = M
local active, stopped = false, false

--- The panel spec /term and ctrl+] open terminals with.
M.defaults = M.defaults or { dock = "bottom", size = 0.4 }
--- Keys that leave a terminal for the prompt; from the prompt, ctrl+] calls
--- M.toggle. Terminals without the kitty keyboard protocol send ctrl+] as
--- the byte crossterm reads as ctrl+5.
M.escape = M.escape or { ["ctrl+]"] = true, ["ctrl+5"] = true }
--- Keys a terminal leaves to the keymaps (the "panel" context), such as a
--- window manager's prefix: M.passthrough["alt+w"] = true.
M.passthrough = M.passthrough or {}

-- id -> { job, screen, scroll }
local terms = {}
-- The terminal last opened or typed into.
local last

local function shift_scroll(t, by)
  local max = t.screen and t.screen.max or 0
  t.scroll = math.max(0, math.min(max, t.scroll + by))
end

local function page_scroll(t, direction)
  local page = math.max(1, (t.screen and #t.screen.lines or 24) - 1)
  shift_scroll(t, direction * page)
end

-- Key names (as on_key gets them) to the bytes a terminal sends.
local CSI = "\27["
local tilde = { insert = 2, delete = 3, pageup = 5, pagedown = 6, f5 = 15, f6 = 17, f7 = 18, f8 = 19, f9 = 20, f10 = 21, f11 = 23, f12 = 24 }
local letter = { up = "A", down = "B", right = "C", left = "D", home = "H", ["end"] = "F", f1 = "P", f2 = "Q", f3 = "R", f4 = "S" }
local plain = { enter = "\r", tab = "\t", backspace = "\127", esc = "\27", space = " " }
local ctrl_punct = { ["@"] = 0, space = 0, ["["] = 27, ["\\"] = 28, ["]"] = 29, ["^"] = 30, ["_"] = 31, ["/"] = 31,
  ["4"] = 28, ["5"] = 29, ["6"] = 30, ["7"] = 31 } -- crossterm's legacy byte aliases

local function encode(key, app_cursor)
  local mods, name = {}, key
  while true do
    local m, rest = name:match("^(%a+)%+(.+)$")
    if not (m and (m == "ctrl" or m == "alt" or m == "shift" or m == "super")) then
      break
    end
    mods[m], name = true, rest
  end
  -- xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
  local param = 1 + (mods.shift and 1 or 0) + (mods.alt and 2 or 0) + (mods.ctrl and 4 or 0)
  if letter[name] then
    if param > 1 then
      return CSI .. "1;" .. param .. letter[name]
    end
    local ss3 = name:match("^f%d") or app_cursor
    return (ss3 and "\27O" or CSI) .. letter[name]
  end
  if tilde[name] then
    return CSI .. tilde[name] .. (param > 1 and (";" .. param) or "") .. "~"
  end
  if name == "tab" and mods.shift then
    return CSI .. "Z"
  end
  local bytes
  if mods.ctrl then
    if ctrl_punct[name] then
      bytes = string.char(ctrl_punct[name])
    elseif name:match("^%a$") then
      bytes = string.char(name:lower():byte() - 96)
    elseif name == "backspace" then
      bytes = "\8"
    end
  end
  bytes = bytes or plain[name] or (#name > 0 and not name:match("^%a%w+$") and name) or nil
  if bytes and mods.alt then
    bytes = "\27" .. bytes
  end
  return bytes
end
M.encode = encode

-- Give the cell at `col` of a line the cursor's look.
local function with_cursor(line, col)
  local out, at, done = {}, 1, false
  for _, item in ipairs(line) do
    local text, hl = item[1], item[2]
    local w = bone.text.width(text)
    if not done and col < at + w then
      local before, cell, after, x = "", nil, "", at
      for ch in text:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
        if cell then
          after = after .. ch
        elseif x + bone.text.width(ch) > col then
          cell = ch
        else
          before = before .. ch
        end
        x = x + bone.text.width(ch)
      end
      if before ~= "" then out[#out + 1] = { before, hl } end
      out[#out + 1] = { cell or " ", "TermCursor" }
      if after ~= "" then out[#out + 1] = { after, hl } end
      done = true
    else
      out[#out + 1] = item
    end
    at = at + w
  end
  if not done then
    out[#out + 1] = { string.rep(" ", col - at) }
    out[#out + 1] = { " ", "TermCursor" }
  end
  return out
end

local function render(id, t, ctx)
  if not active or terms[id] ~= t then return {} end
  local s = t.job:screen({ width = ctx.width, height = ctx.height, scroll = t.scroll }) or t.screen
  if not s then return {} end
  t.screen, t.scroll = s, s.scroll
  local lines = {}
  for i, line in ipairs(s.lines) do lines[i] = line end
  if s.cursor and ctx.focused then
    local row, col = s.cursor[1], s.cursor[2]
    lines[row] = with_cursor(lines[row] or {}, col)
  end
  return lines
end

-- Desktop app: the space around the screen, in pixels.
local PAD_X, PAD_Y = 6, 4
local CHAR = "[%z\1-\127\194-\244][\128-\191]*"

local function codepoint(ch)
  local b1, b2, b3, b4 = ch:byte(1, 4)
  b2, b3, b4 = b2 or 0x80, b3 or 0x80, b4 or 0x80
  if b1 < 0x80 then return b1 end
  if b1 < 0xE0 then return (b1 % 0x20) * 0x40 + b2 % 0x40 end
  if b1 < 0xF0 then return (b1 % 0x10) * 0x1000 + (b2 % 0x40) * 0x40 + b3 % 0x40 end
  return (b1 % 0x08) * 0x40000 + (b2 % 0x40) * 0x1000 + (b3 % 0x40) * 0x40 + b4 % 0x40
end

-- Characters a monospace font draws one cell wide: Latin, Greek, Cyrillic,
-- punctuation, box drawing and blocks. Any other one (wide, an icon, an
-- emoji, one from a fallback font) is placed in its cells on its own, so a
-- glyph that is not exactly as wide cannot move the rest of the line.
local function regular(ch)
  if #ch == 1 then return true end
  local cp = codepoint(ch)
  return cp < 0x530 or (cp >= 0x2000 and cp < 0x2070) or (cp >= 0x2500 and cp < 0x25A0)
end

local function char_width(ch)
  if #ch == 1 then return 1 end
  return bone.text.width(ch)
end

-- A line's characters by column: cells[col] is the character there, "" for
-- the second cell of a wide one.
local function cells_of(line)
  local cells = {}
  for _, item in ipairs(line or {}) do
    for ch in item[1]:gmatch(CHAR) do
      local w = char_width(ch)
      if w == 0 and #cells > 0 then
        local i = #cells
        while i > 1 and cells[i] == "" do i = i - 1 end
        cells[i] = cells[i] .. ch
      else
        cells[#cells + 1] = ch
        for _ = 2, w do cells[#cells + 1] = "" end
      end
    end
  end
  return cells
end

-- Selection: t.sel = { a = { line, col }, b = { line, col }, unit = 1 | 2 | 3
-- (characters, words, lines) }. A line is a screen row minus the scroll, so
-- a selection stays on its text while scrolling back.
local WORD_STOP = "[%s\"'`()%[%]{}<>|,;]"

local function word_edge(t, line, col, dir)
  local cells = cells_of(t.screen and t.screen.lines[line + t.scroll])
  local ch = cells[col]
  if not ch or ch == "" and col > 1 then
    while col > 1 and cells[col] == "" do col = col - 1 end
    ch = cells[col]
  end
  if not ch or ch:match("^" .. WORD_STOP .. "$") then return col end
  while true do
    local n = col + dir
    while cells[n] == "" do n = n + dir end
    local c = cells[n]
    if not c or c:match("^" .. WORD_STOP .. "$") then break end
    col = n
  end
  if dir > 0 then while cells[col + 1] == "" do col = col + 1 end end
  return col
end

-- The selection's first and last cell, in order: l1, c1, l2, c2.
local function sel_range(t)
  local s = t.sel
  if not s then return end
  local a, b = s.a, s.b
  if a[1] > b[1] or (a[1] == b[1] and a[2] > b[2]) then a, b = b, a end
  local l1, c1, l2, c2 = a[1], a[2], b[1], b[2]
  if s.unit == 2 then
    c1, c2 = word_edge(t, l1, c1, -1), word_edge(t, l2, c2, 1)
  elseif s.unit == 3 then
    c1, c2 = 1, math.huge
  end
  return l1, c1, l2, c2
end

-- The selected text: each line's cells, trailing blanks dropped.
local function sel_text(t)
  local l1, c1, l2, c2 = sel_range(t)
  if not l1 or not t.screen then return nil end
  local out = {}
  for line = l1, l2 do
    local cells = cells_of(t.screen.lines[line + t.scroll])
    local from, to = line == l1 and c1 or 1, line == l2 and math.min(c2, #cells) or #cells
    local parts = {}
    for col = from, to do parts[#parts + 1] = cells[col] end
    out[#out + 1] = (table.concat(parts):gsub("%s+$", ""))
  end
  local text = table.concat(out, "\n")
  return text ~= "" and text or nil
end

-- "@term/<fg>/<bg>/<flags>" (see bone-ui's term.rs) drawn in reverse: a group
-- with the colors swapped, the defaults being Normal's.
local reversed = {}
local function reverse_group(fg, bg, flags, normal)
  local name = "@termrev/" .. fg .. "/" .. bg .. "/" .. flags
  local key = tostring(normal.fg) .. tostring(normal.bg)
  if reversed[name] ~= key then
    local function value(c, default) return c == "-" and default or tonumber(c) or c end
    bone.hl.set(name, {
      fg = value(bg, normal.bg or "#000000"), bg = value(fg, normal.fg or "#eeeeee"),
      bold = flags:find("b") and true or nil, italic = flags:find("i") and true or nil,
      underline = flags:find("u") and true or nil,
    })
    reversed[name] = key
  end
  return name
end

-- A run of cells with one look, as a box exactly that many cells wide.
local function segment(seg, look)
  local width = seg.cols * look.cw
  local text = table.concat(seg.parts)
  local hl, bg = seg.hl ~= "Normal" and seg.hl or nil, nil
  local fg, b, flags = seg.hl:match("^@term/([^/]*)/([^/]*)/(%a*)$")
  if fg and flags:find("r") then
    hl = reverse_group(fg, b, flags, look.normal)
    bg = hl
  elseif fg and b ~= "-" then
    bg = seg.hl
  end
  if seg.sel then bg = look.selection end
  local span = { text = text, hl = hl }
  if seg.cur then
    bg, span.color = look.normal.fg or "#eeeeee", look.normal.bg or "#161616"
  end
  if not bg and not text:find("[^ ]") then
    return { width = width, height = look.ch, shrink = 0 }
  end
  return {
    width = width, height = look.ch, shrink = 0, bg = bg, line_height = look.ch,
    font = "mono", size = look.size, wrap = false, spans = { span },
  }
end

-- One screen row: its runs of cells, with the selection and the cursor.
local function row_node(t, line, row, cursor, sel, look)
  local kids, seg, col = { direction = "row", height = look.ch, shrink = 0 }, nil, 1
  local l1, c1, l2, c2 = sel[1], sel[2], sel[3], sel[4]
  local selected = l1 and line >= l1 and line <= l2
  local function in_sel(c)
    return selected and (line > l1 or c >= c1) and (line < l2 or c <= c2)
  end
  local function flush()
    if seg then kids[#kids + 1] = segment(seg, look) end
    seg = nil
  end
  local function add(ch, w, hl, lone)
    local s, cur = in_sel(col), cursor == col
    local key = hl .. (s and "|s" or "")
    if lone or cur or not seg or seg.lone or seg.cur or seg.key ~= key then
      flush()
      seg = { key = key, hl = hl, sel = s, cur = cur, lone = lone, parts = {}, cols = 0 }
    end
    seg.parts[#seg.parts + 1] = ch
    seg.cols = seg.cols + w
    col = col + w
  end
  for _, item in ipairs(row) do
    local hl = item[2] or "Normal"
    for ch in item[1]:gmatch(CHAR) do
      local w = char_width(ch)
      if w == 0 and seg then
        seg.parts[#seg.parts + 1] = ch
      else
        w = math.max(w, 1)
        add(ch, w, hl, not regular(ch) or w > 1)
      end
    end
  end
  -- The selection and the cursor reach past the end of the text.
  local to = cursor or 0
  if selected then to = math.max(to, line < l2 and look.cols or math.min(c2, look.cols)) end
  while col <= to do add(" ", 1, "Normal", false) end
  flush()
  return kids
end

-- The desktop app's view: the screen on a grid of character cells.
local function node(id, t, ctx)
  local cell = ctx.cell or bone.gui.window_info().cell or { w = 8, h = 18 }
  local cols = math.max(1, math.floor((ctx.width - 2 * PAD_X) / cell.w))
  local rows = math.max(1, math.floor((ctx.height - 2 * PAD_Y) / cell.h))
  local out = { id = "term:" .. id, direction = "column", overflow = "hidden", padding = { PAD_Y, PAD_X } }
  if not active or terms[id] ~= t then return out end
  local s = t.job:screen({ width = cols, height = rows, scroll = t.scroll }) or t.screen
  if not s then return out end
  t.screen, t.scroll, t.cell, t.cols, t.rows = s, s.scroll, cell, cols, rows
  local sel_hl = bone.hl.get("TextSelection")
  local look = {
    cw = cell.w, ch = cell.h, cols = cols, size = bone.o.font_size or 14,
    normal = bone.hl.get("Normal") or {},
    selection = sel_hl and sel_hl.bg and "TextSelection" or "#2a3f55",
  }
  local sel = { sel_range(t) }
  local cursor = ctx.focused and s.cursor
  for r, row in ipairs(s.lines) do
    out[#out + 1] = row_node(t, r - t.scroll, row, cursor and cursor[1] == r and cursor[2], sel, look)
  end
  return out
end

local function on_key(id, t, key)
  if not active or terms[id] ~= t then
    return false
  end
  last = id
  if M.passthrough[key] then
    return false
  elseif key == "ctrl+c" and t.sel then
    -- With a selection, ctrl+c copies it (and ends it) instead of
    -- interrupting the program. Key names fold shift into letters, so
    -- ctrl+shift+c arrives as this too.
    local text = sel_text(t)
    if text then bone.clipboard.copy(text) end
    t.sel = nil
  elseif M.escape[key] then
    bone.ui.panel.focus(nil)
  elseif key == "wheelup" then
    shift_scroll(t, 3)
  elseif key == "wheeldown" then
    shift_scroll(t, -3)
  elseif key == "shift+pageup" or (key == "pageup" and t.scroll > 0) then
    page_scroll(t, 1)
  elseif key == "shift+pagedown" or (key == "pagedown" and t.scroll > 0) then
    page_scroll(t, -1)
  elseif key == "ctrl+home" then
    shift_scroll(t, 1e9)
  elseif key == "ctrl+end" then
    t.scroll = 0
  else
    local bytes = encode(key, t.screen and t.screen.app_cursor)
    if bytes then
      t.scroll, t.sel = 0, nil
      t.job:write(bytes)
    end
  end
  return true
end

local pasting
local mouse_events
-- The terminal being selected in with the pointer, and the last press.
local selecting
local last_press

-- In the desktop app: the cell under the pointer of terminal `t`, from its
-- grid's place on screen (unclamped; nil when it has not been drawn).
local function gui_cell(id, t, ev)
  local r = t.cell and ev.px and bone.gui.rect("term:" .. id)
  if not r then return end
  local col = math.floor((ev.px - r.x - PAD_X) / t.cell.w) + 1
  local row = math.floor((ev.py - r.y - PAD_Y) / t.cell.h) + 1
  return row, col, r
end

local function mouse_position(ev)
  if ev.px and bone.gui and bone.gui.rect then
    for id, t in pairs(terms) do
      local row, col, r = gui_cell(id, t, ev)
      if row and t.screen and ev.px >= r.x and ev.px < r.x + r.w and ev.py >= r.y and ev.py < r.y + r.h then
        return id, t, math.max(1, math.min(t.rows, row)), math.max(1, math.min(t.cols, col))
      end
    end
    return
  end
  local id = ev.panel
  if not id and type(ev.region) == "string" then
    id = ev.region:match("^panel:(.+)$")
  end
  if not id then return end
  local t = terms[id]
  if not t or not t.screen then return end
  local row = ev.panel_row or ev.row
  local col = ev.panel_col or ev.col
  if not (row and col and row > 0 and col > 0) then return end
  return id, t, row, col
end

-- A drag in progress: extend the selection, scrolling back past the top.
local function drag_selection(id, t, ev)
  local row, col = gui_cell(id, t, ev)
  if not row then return end
  if row < 1 then
    shift_scroll(t, 1)
  elseif row > t.rows and t.scroll > 0 then
    shift_scroll(t, -1)
  end
  row = math.max(1, math.min(t.rows, row))
  col = math.max(1, math.min(t.cols, col))
  t.sel.b = { row - t.scroll, col }
end

-- Press, drag and release of the left button where the program does not
-- take the mouse: select, and copy on release.
local function select_mouse(ev)
  local t = selecting and terms[selecting]
  if t and t.sel and (ev.action == "drag" or ev.action == "up") then
    drag_selection(selecting, t, ev)
    if ev.action == "up" then
      selecting = nil
      local a, b = t.sel.a, t.sel.b
      local click = t.sel.unit == 1 and a[1] == b[1] and a[2] == b[2]
      local text = not click and sel_text(t)
      if text then
        bone.clipboard.copy(text)
      else
        t.sel = nil
      end
    end
    return true
  end
  selecting = nil
end

local function mouse_code(t, ev, row, col)
  local mode = t.screen.mouse
  if mode == nil or mode == "none" then return end
  local action, button = ev.action, ev.button
  if action == "up" and mode == "press" then return end
  if (action == "drag" and mode ~= "button_motion" and mode ~= "any_motion") or (action == "move" and mode ~= "any_motion") then return end
  local code
  if action == "wheel" then
    code = button == "up" and 64 or 65
  else
    code = button and ({ left = 0, middle = 1, right = 2 })[button] or (action == "move" and 35)
    if code == nil then return end
    if action == "up" then
      code = 3
    elseif action == "drag" then
      code = code + 32
    end
  end
  if t.screen.mouse_encoding == "sgr" then
    return string.format("\27[<%d;%d;%d%s", code, col, row, action == "up" and "m" or "M")
  end
  if t.screen.mouse_encoding == "utf8" then -- LuaJIT has no utf8 library: two bytes past 127
    local function u(n) return n < 128 and string.char(n) or string.char(192 + math.floor(n / 64), 128 + n % 64) end
    return "\27[M" .. u(32 + code) .. u(32 + col) .. u(32 + row)
  end
  return "\27[M" .. string.char(32 + code, 32 + math.min(223, col), 32 + math.min(223, row))
end

function M.setup()
  if stopped or pasting then
    return
  end
  active, M._active = true, true
  pasting = bone.on("paste", function(ev)
    local t = terms[bone.ui.panel.focused() or ""]
    if not active or ev.context ~= "panel" or not t or type(ev.text) ~= "string" then
      return
    end
    -- Line breaks are typed as enter, as terminals paste them; the bracket's
    -- own end in the text would end the paste early and run the rest.
    local text = ev.text:gsub("\r?\n", "\r")
    if t.screen and t.screen.bracketed_paste then
      text = "\27[200~" .. text:gsub("\27%[20[01]~", "") .. "\27[201~"
    end
    t.scroll, t.sel = 0, nil
    t.job:write(text)
  end)
  mouse_events = bone.on("mouse", function(ev)
    if active and select_mouse(ev) then return true end
    local id, t, row, col = mouse_position(ev)
    if not id then return end
    local bytes = mouse_code(t, ev, row, col)
    if bytes then
      bone.ui.panel.focus(id)
      t.scroll, t.sel = 0, nil
      t.job:write(bytes)
      return true
    end
    if ev.action == "wheel" then
      shift_scroll(t, ev.button == "up" and 3 or -3); if bone.gui then bone.gui.invalidate() end; return true
    end
    -- The desktop app selects; the TUI's own selection works over panels.
    if ev.action == "down" and ev.button == "left" and ev.px and t.cell then
      local now, line = bone.now(), row - t.scroll
      local p = last_press
      local unit = 1
      if p and p.id == id and now - p.time < 400 and p.line == line and p.col == col then
        unit = p.unit % 3 + 1
      end
      last_press = { id = id, time = now, line = line, col = col, unit = unit }
      t.sel = { a = { line, col }, b = { line, col }, unit = unit }
      selecting = id
      bone.ui.panel.focus(id)
      return true
    end
  end)
end

--- The shell a terminal runs when given no command, as an argv list:
--- $SHELL, else /bin/sh. On Windows $SHELL counts only when it names a file
--- there (Git Bash sets a Unix one); else PowerShell 7 (pwsh) when it is on
--- the PATH, then Windows PowerShell, then %ComSpec% (cmd).
function M.shell()
  local function file(path)
    local f = path and path ~= "" and io.open(path, "rb")
    if f then f:close() return true end
    return false
  end
  local env = os.getenv("SHELL")
  local windows = package.config:sub(1, 1) == "\\"
  if not windows then return { (env and env ~= "") and env or "/bin/sh" } end
  if file(env) then return { env } end
  for dir in (os.getenv("PATH") or ""):gmatch("[^;]+") do
    if file(dir .. "\\pwsh.exe") then return { dir .. "\\pwsh.exe", "-NoLogo" } end
  end
  local root = os.getenv("SystemRoot") or "C:\\Windows"
  local ps = root .. "\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
  if file(ps) then return { ps, "-NoLogo" } end
  return { os.getenv("ComSpec") or "cmd.exe" }
end

--- Open a terminal in a panel. `spec`: panel options over M.defaults (an `id`
--- of a "panel:<id>" layout leaf puts it there). `opts`: { cmd (a shell
--- command or an argv list; default M.shell()), cwd (default: the session's) }.
--- Returns the panel's handle; closing the panel stops the program, and the
--- shell ending closes the panel (a command's output stays until it is closed).
-- Installed once, so a window manager's saved term_open remains valid.
if not M.open then
  M.open = function(spec, opts) return M._open(spec, opts) end
end

local function release(id, t)
  if terms[id] ~= t then return end
  terms[id] = nil -- Invalidate before cancel, which can trigger on_exit.
  if last == id then
    last = next(terms)
  end
  if t.job then t.job:cancel() end
end

function M._open(spec, opts)
  if not active then return nil end
  opts = opts or {}
  local p = {}
  for k, v in pairs(M.defaults) do p[k] = v end
  for k, v in pairs(spec or {}) do p[k] = v end
  if not p.id then
    local n = 1
    while bone.ui.panel.get("term" .. n) do n = n + 1 end
    p.id = "term" .. n
  end
  local id = p.id
  -- Do not cancel a live job then fail opening its still-existing panel.
  -- Explicit duplicate ids are errors, just like bone.ui.panel.open.
  assert(not bone.ui.panel.get(id), "panel " .. id .. " is already open")
  bone.hl.set("TermCursor", { link = "Normal", reverse = true })
  local session = bone.chat.session()
  local t = { scroll = 0 }
  local shell = M.shell()
  t.job = bone.job.start(opts.cmd or shell, {
    name = id,
    pty = { width = 80, height = 24 },
    cwd = opts.cwd or (session and session.cwd),
    on_exit = function(r)
      if not active or terms[id] ~= t then
        return
      elseif r.error or (opts.cmd and r.code ~= 0) then
        bone.notify(r.error or ((r.cmd or id) .. " exited with " .. (r.code or ("signal " .. tostring(r.signal)))), "error")
      end
      if not opts.cmd then
        bone.ui.panel.close(id)
      else
        t.screen = t.job:screen() or t.screen -- Keep a final screen after job-cache expiry.
      end
    end,
  })
  terms[id] = t
  last = id
  p.focusable = true
  p.title = p.title or "terminal · " .. (type(opts.cmd) == "table" and table.concat(opts.cmd, " ") or opts.cmd or shell[1]:match("[^/\\\\]+$"))
  if p.focus == nil then p.focus = true end
  p.render = function(ctx) return render(id, t, ctx) end
  p.node = function(ctx) return node(id, t, ctx) end
  p.on_key = function(key) return on_key(id, t, key) end
  local on_close = p.on_close
  p.on_close = function(panel)
    if terms[id] ~= t then return end
    release(id, t)
    if on_close then on_close(panel) end
  end
  local ok, panel = pcall(function()
    local panel = bone.ui.panel.open(p)
    local attach = not spec and require("bone.nodes.window").attach_panel
    if attach then attach(id) end
    return panel
  end)
  if not ok then
    pcall(bone.ui.panel.close, id)
    release(id, t)
    error(panel, 0)
  end
  return panel
end

--- The terminal's job handle, for writing to it or reading its screen.
function M.job(id)
  return terms[id] and terms[id].job
end

--- Close terminal `id` (default: the focused one, else the last), which
--- stops its program. Returns whether one was open.
function M.close(id)
  local focused = bone.ui.panel.focused()
  id = id or (terms[focused or ""] and focused) or last
  if not (id and terms[id]) then
    return false
  end
  local t = terms[id]
  local ok, closed = pcall(bone.ui.panel.close, id)
  -- Also clean up if the panel was already removed or its on_close failed.
  release(id, t)
  if not ok then error(closed, 0) end
  return closed
end

--- Close every terminal, including ones opened from another plugin's callback.
--- Bone's owner is the caller in that case, so automatic ownership alone is
--- not enough to stop those jobs when term unloads. Snapshot before callbacks
--- mutate the table, and continue cleanup even if a caller's on_close fails.
function M.close_all()
  local snapshot = {}
  for id, t in pairs(terms) do snapshot[#snapshot + 1] = { id, t } end
  for _, entry in ipairs(snapshot) do
    local id, t = entry[1], entry[2]
    if terms[id] == t then
      pcall(bone.ui.panel.close, id)
      pcall(release, id, t)
    end
  end
  last = nil
end

--- Deactivate callbacks before closing panels/cancelling jobs. Idempotent.
function M.shutdown()
  if not active then return end
  active, stopped, M._active = false, true, false
  -- Another plugin (or an older runtime's defaults) may have required us
  -- first, so Bone's per-plugin module ownership need not include this name.
  -- Explicitly evict this generation; the stable facade still serves wrappers.
  if package.loaded["bone.terminal"] == M then
    package.loaded["bone.terminal"] = nil
  end
  local handler = pasting
  pasting = nil
  if handler then pcall(bone.off, handler) end
  handler = mouse_events
  mouse_events = nil
  if handler then pcall(bone.off, handler) end
  M.close_all()
end

--- Focus the last terminal (showing it if hidden), or open one.
function M.toggle()
  if not active then return nil end
  local panel = last and terms[last] and bone.ui.panel.get(last)
  if not panel then
    return M.open()
  end
  panel:show()
  panel:focus()
  return panel
end

return M
