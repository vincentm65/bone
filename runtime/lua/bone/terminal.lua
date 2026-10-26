-- Terminals (/term, ctrl+]) for the GUI: a shell, or any command, in a
-- pseudo-terminal (bone.job with pty), drawn as a panel's content. It is
-- the term plugin's module, over shared APIs only.
--
-- A shell, or any command, in a pseudo-terminal,
-- drawn as a panel's content. Where it goes is the panel's business: any
-- bone.ui.panel option (dock, size, hidden, …) passes through, or put
-- "panel:<id>" in bone.ui.layout. While it has the keyboard every key goes to
-- the program except those in M.escape, which gives the keyboard back to the prompt;
-- shift+pageup/pagedown scroll back.
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

-- Node panels use pixels; the PTY and render() still use character cells.
local function node(id, t, ctx)
  local cell = ctx.cell or bone.gui.window_info().cell or { w = 8, h = 18 }
  local lines = render(id, t, {
    width = math.max(1, math.floor(ctx.width / cell.w)),
    height = math.max(1, math.floor(ctx.height / cell.h)),
    focused = ctx.focused,
  })
  local out = { direction = "column", overflow = "hidden" }
  local normal = bone.hl.get("Normal") or {}
  for _, line in ipairs(lines) do
    local spans = {}
    for i, item in ipairs(line) do
      local span = { text = item[1], hl = item[2], bg = item[2] ~= "Normal" and item[2] or nil }
      -- Node spans do not apply the TUI's reverse modifier.
      if item[2] == "TermCursor" then
        span.color, span.bg = normal.bg or "#161616", normal.fg or "#eeeeee"
      end
      spans[i] = span
    end
    out[#out + 1] = {
      height = cell.h, line_height = cell.h, font = "mono", size = bone.o.font_size or 14,
      wrap = false, spans = #spans > 0 and spans or { " " },
    }
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
  elseif M.escape[key] then
    bone.ui.panel.focus(nil)
  elseif key == "shift+pageup" or key == "shift+pagedown" then
    local page = math.max(1, (t.screen and #t.screen.lines or 24) - 1)
    shift_scroll(t, key == "shift+pageup" and page or -page)
  else
    local bytes = encode(key, t.screen and t.screen.app_cursor)
    if bytes then
      t.scroll = 0
      t.job:write(bytes)
    end
  end
  return true
end

local pasting
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
    local text = ev.text
    if t.screen and t.screen.bracketed_paste then
      text = "\27[200~" .. text .. "\27[201~"
    end
    t.scroll = 0
    t.job:write(text)
  end)
end

--- Open a terminal in a panel. `spec`: panel options over M.defaults (an `id`
--- of a "panel:<id>" layout leaf puts it there). `opts`: { cmd (a shell
--- command or an argv list; default $SHELL), cwd (default: the session's) }.
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
  t.job = bone.job.start(opts.cmd or { os.getenv("SHELL") or "/bin/sh" }, {
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
  p.title = p.title or "terminal · " .. (type(opts.cmd) == "table" and table.concat(opts.cmd, " ") or opts.cmd or os.getenv("SHELL") or "/bin/sh")
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
