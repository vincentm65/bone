-- The desktop half of the files plugin: a file explorer, a file viewer and
-- a text editor, in two windows. The explorer walks the folders on the
-- left and shows what the selected file holds on the right (Markdown,
-- HTML, a PDF's text, an image, or the file's own text); the editor is a
-- window of its own, over the explorer, and writes the file itself when
-- it is saved.

local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local kinds = require("files.kinds")
local html = require("files.html")
local text = require("files.text")
local markdown = require("bone.desktop.markdown")

local TREE_ID = "files:tree"
local PREVIEW_ID = "files:preview"
local EDITOR_ID = "files:editor"
local EDITOR_PREVIEW_ID = "files:editor:preview"
local EXPLORER_CARD_ID = "files:explorer-card"
local EDITOR_CARD_ID = "files:editor-card"
local EXPLORER_WIDTH_STATE = "files-explorer-width"
local EDITOR_WIDTH_STATE = "files-editor-width"
local MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH = 72, 180

local function saved_width(name, default)
  local width = tonumber(bone.state.load(name))
  return math.max(MIN_WINDOW_WIDTH, math.min(MAX_WINDOW_WIDTH, width or default))
end

local explorer_width = saved_width(EXPLORER_WIDTH_STATE, 120)
local editor_width = saved_width(EDITOR_WIDTH_STATE, 120)
local explorer_resize, editor_resize = {}, {}

-- The explorer: the folder it is in, what is in it, and what is shown.
local explorer = {
  root = "",
  entries = {},
  selected = 1,
  hidden = false,
  preview = nil,
  error = nil,
  note = nil,
}

-- The editor: the file it has open and what was done to it.
local editor = {
  path = "",
  lines = { "" },
  trailing = false,
  original = "",
  line = 1,
  col = 0,
  dirty = false,
  status = nil,
  error = nil,
  saving = false,
  confirm = false,
  preview = false,
  md_text = nil,
  md_nodes = nil,
  undo = {},
  redo = {},
  undo_kind = nil,
  undo_row = nil,
}

--- How many steps the editor keeps to take back.
local UNDO_MAX = 100

local explorer_window, editor_window
local scrolled = {} -- list id -> the entry last scrolled into view

-- The windows' content and their keys, and the two helpers that redraw
-- them: defined below, in the order they read best.
local refresh_explorer, refresh_editor
local explorer_node, explorer_key, editor_node, editor_key

-- ---- paths ----------------------------------------------------------------

local function join(dir, name)
  if dir == "" then return name end
  if dir:sub(-1) == "/" then return dir .. name end
  return dir .. "/" .. name
end

--- The folder above `dir`.
local function parent(dir)
  if dir == "/" or dir == "" then return dir end
  local clean = dir:gsub("/+$", "")
  local up = clean:match("^(.*)/[^/]+$")
  if not up or up == "" then return "/" end
  return up
end

--- Where the explorer starts: the conversation's own directory.
local function home()
  local ok, session = pcall(bone.api.session)
  if ok and type(session) == "table" and session.cwd and session.cwd ~= "" then
    return session.cwd
  end
  return os.getenv("HOME") or "/"
end

--- `path` as an absolute one (what the tools want).
local function absolute(path)
  path = tostring(path or ""):match("^%s*(.-)%s*$")
  if path == "" then return home() end
  if path == "~" then
    path = os.getenv("HOME") or "/"
  elseif path:sub(1, 2) == "~/" then
    path = (os.getenv("HOME") or "/") .. path:sub(2)
  end
  if path:sub(1, 1) ~= "/" then path = join(home(), path) end
  return path
end

-- ---- the explorer ---------------------------------------------------------

--- What is in `dir`, folders first.
local function load(dir)
  local list, err = bone.fs.list(dir)
  if not list then
    explorer.entries = {}
    explorer.selected = 1
    explorer.error = tostring(err or ("cannot read " .. dir))
    return
  end
  local entries = {}
  for _, entry in ipairs(list) do
    if explorer.hidden or entry.name:sub(1, 1) ~= "." then
      entries[#entries + 1] = {
        name = entry.name, type = entry.type,
        size = entry.size, modified = entry.modified,
      }
    end
  end
  table.sort(entries, function(a, b)
    if a.type ~= b.type then return a.type == "dir" end
    return a.name:lower() < b.name:lower()
  end)
  explorer.entries = entries
  explorer.selected = math.max(1, math.min(explorer.selected, math.max(#entries, 1)))
  explorer.error = nil
end

local function format_size(value)
  local size = tonumber(value)
  if not size then return "—" end
  local units = { "B", "KB", "MB", "GB", "TB" }
  local unit = 1
  while size >= 1024 and unit < #units do
    size, unit = size / 1024, unit + 1
  end
  if unit == 1 then return string.format("%d B", size) end
  return string.format("%.1f %s", size, units[unit])
end

local function format_date(value)
  local stamp = tonumber(value)
  if not stamp then return "unknown date" end
  local ok, date = pcall(os.date, "%Y-%m-%d %H:%M", stamp)
  return ok and date or "unknown date"
end

local function cell_width()
  local ok, info = pcall(bone.gui.window_info)
  return ok and info and info.cell and tonumber(info.cell.w) or 8
end

local function content_width(cells)
  return math.max(320, cells * cell_width() - 28)
end

local function resize_handle(card_id, window_ref, width, set_width, state_name, state)
  local handle_width = 8
  return {
    position = "absolute", top = 0, bottom = 0, right = 0, width = handle_width, z = 2,
    cursor = "col-resize",
    on_press = function(pos)
      local card = bone.gui.rect(card_id)
      -- Press coordinates are local to the handle; drag coordinates are in
      -- the window, so convert the initial point to the same coordinate
      -- space before calculating the width delta.
      state.start_x = card and (card.x + card.w - handle_width + pos.x) or nil
      state.start_width = width()
    end,
    on_drag = function(pos)
      if state.start_x == nil then return end
      local next_width = math.floor(state.start_width
        + (pos.x - state.start_x) / cell_width() + 0.5)
      next_width = math.max(MIN_WINDOW_WIDTH, math.min(MAX_WINDOW_WIDTH, next_width))
      if next_width == width() then return end
      set_width(next_width)
      local window = window_ref()
      if window and bone.ui.is_open(window) then
        bone.ui.update(window, { width = next_width })
      end
    end,
    on_release = function()
      state.start_x = nil
      bone.state.save(state_name, width())
    end,
  }
end

--- The text of a PDF, through the core (it reads the PDF itself).
local function ask_pdf_text(path, preview)
  bone.rpc.call("files.pdftext", { path = path }, function(result, err)
    if explorer.preview ~= preview then return end
    if result and result.text then
      preview.text = result.text
      preview.lines = text.split(result.text)
      preview.state = "ready"
    else
      preview.error = (result and result.error)
        or (err and (type(err) == "table" and err.message or tostring(err)))
        or "The PDF could not be read."
      preview.state = "ready"
    end
    refresh_explorer()
  end)
end

--- What the file holds, to show it.
local function show_preview(entry)
  local path = join(explorer.root, entry.name)
  local kind = kinds.of(entry.name)
  local preview = { path = path, name = entry.name, kind = kind }
  explorer.preview = preview
  if kind == "image" then return end
  if kind == "pdf" then
    preview.state = "loading"
    ask_pdf_text(path, preview)
    return
  end
  local content, err = text.read(path)
  if not content then
    preview.error = err
    return
  end
  if text.is_binary(content) then
    preview.error = "This is a binary file: there is nothing to show."
    return
  end
  local body, cut = text.for_preview(content)
  if cut then preview.note = "The file is large: only its first part is shown." end
  if kind == "html" then
    body = html.to_text(body)
  elseif kind == "binary" then
    kind = "code"
    preview.kind = kind
  end
  preview.lines = text.split(body)
  if kind == "markdown" then
    preview.nodes = markdown.nodes(body, "Normal", { size = style.size.caption })
  end
end

local function move(by)
  local count = #explorer.entries
  if count == 0 then return end
  explorer.selected = math.max(1, math.min(count, explorer.selected + by))
end

--- Open what is selected: a folder to walk into, a file to look at.
local function activate(index)
  local entry = explorer.entries[index]
  if not entry then return end
  explorer.selected = index
  if entry.type == "dir" then
    explorer.root = join(explorer.root, entry.name)
    explorer.entries = {}
    explorer.selected = 1
    explorer.preview = nil
    load(explorer.root)
  else
    show_preview(entry)
  end
end

--- One folder up.
local function go_up()
  local up = parent(explorer.root)
  if up == explorer.root then return end
  explorer.root = up
  explorer.entries = {}
  explorer.selected = 1
  explorer.preview = nil
  load(explorer.root)
end

--- Open the file with the desktop's own app.
local function open_externally()
  local entry = explorer.entries[explorer.selected]
  if not entry then return end
  local path = join(explorer.root, entry.name)
  explorer.note = "Opening " .. entry.name .. "…"
  refresh_explorer()
  bone.rpc.call("files.open", { path = path }, function(result, err)
    if result and result.opened then
      explorer.note = "Opened " .. entry.name
    else
      explorer.note = nil
      bone.notify((result and result.error)
        or (err and (type(err) == "table" and err.message or tostring(err)))
        or ("Could not open " .. entry.name .. "."), "error")
    end
    refresh_explorer()
  end)
end

-- ---- the windows ----------------------------------------------------------

refresh_explorer = function()
  if explorer_window and bone.ui.is_open(explorer_window) then
    bone.ui.update(explorer_window, {})
  end
end

refresh_editor = function()
  if editor_window and bone.ui.is_open(editor_window) then
    bone.ui.update(editor_window, {})
  end
end

--- Keep the entry that is selected in view (once per change: the reader
--- may scroll the list away from it).
local function scroll_to(id, index)
  if scrolled[id] == index then return end
  if bone.gui.scroll_item(id, index, "nearest") then scrolled[id] = index end
end

local function entry_node(index)
  local entry = explorer.entries[index]
  if not entry then return { text = "" } end
  local on = index == explorer.selected
  local details = format_date(entry.modified)
  if entry.type == "file" then
    details = format_size(entry.size) .. " · " .. details
  end
  return widgets.entry(on, {
    { direction = "row", align = "center", gap = 8, width = 0, grow = 1,
      { text = (entry.type == "dir" and "▸ " or "  ") .. entry.name,
        hl = entry.type == "dir" and "Accent" or "Normal",
        size = style.size.caption, bold = on, wrap = false, grow = 1, width = 0,
        text_overflow = "ellipsis" },
      { text = details, hl = "Dim", size = style.size.micro, wrap = false, shrink = 0 },
    },
  }, function() activate(index) end)
end

local function tree_node()
  if #explorer.entries == 0 then
    return { text = "(empty)", hl = "Dim", size = style.size.caption, padding = { 6, 10 } }
  end
  return {
    id = TREE_ID,
    grow = 1,
    list = {
      count = #explorer.entries,
      estimate = 26,
      key = "files:tree:" .. explorer.root .. (explorer.hidden and ":hidden" or ""),
      item = function(index) return entry_node(index) end,
    },
  }
end

local function preview_node()
  local preview = explorer.preview
  if not preview then
    return { text = "Select a file to see it here.", hl = "Dim", size = style.size.small,
      padding = { 10, 12 } }
  end
  if preview.error then
    return { text = preview.error, hl = "ErrorMsg", size = style.size.small,
      padding = { 10, 12 }, selectable = true }
  end
  if preview.kind == "image" then
    return {
      grow = 1, align = "center", justify = "center",
      { image = { path = preview.path, width = 720, height = 520, fit = "contain" } },
    }
  end
  if preview.kind == "pdf" and preview.state ~= "ready" then
    return { text = "Reading the PDF…", hl = "Dim", size = style.size.small, padding = { 10, 12 } }
  end
  if preview.nodes then
    return {
      id = PREVIEW_ID, grow = 1,
      list = {
        count = #preview.nodes, estimate = 26,
        key = "files:preview:" .. preview.path,
        item = function(index) return preview.nodes[index] end,
      },
    }
  end
  local lines = preview.lines or { "" }
  return {
    id = PREVIEW_ID, grow = 1,
    list = {
      count = #lines, estimate = 20,
      key = "files:preview:" .. preview.path,
      item = function(index)
        return { text = lines[index], font = "mono", size = style.size.small,
          wrap = true, selectable = true }
      end,
    },
  }
end

explorer_node = function()
  local root = { id = EXPLORER_CARD_ID, position = "relative", direction = "column", gap = 0,
    grow = 1, max_width = content_width(explorer_width), max_height = 720 }

  local header = { direction = "row", align = "center", gap = style.space.md, padding = { 0, 0, 10, 0 } }
  header[#header + 1] = { text = "Files", bold = true, size = style.size.title }
  header[#header + 1] = { text = explorer.root, hl = "Dim", size = style.size.small,
    grow = 1, wrap = false }
  if explorer.error then
    header[#header + 1] = { text = explorer.error, hl = "ErrorMsg", size = style.size.small }
  elseif explorer.note then
    header[#header + 1] = { text = explorer.note, hl = "Dim", size = style.size.small }
  end
  root[#root + 1] = header

  local body = { direction = "row", gap = 12, grow = 1 }
  local left = { direction = "column", width = 300, gap = 4 }
  left[#left + 1] = tree_node()
  body[#body + 1] = left
  body[#body + 1] = { width = 1, bg = style.alpha("PromptBorder", "", "fg", "#3a3a3a") }
  local right = { direction = "column", gap = 6, grow = 1 }
  if explorer.preview then
    right[#right + 1] = { text = explorer.preview.name, size = style.size.caption,
      bold = true, wrap = false }
  end
  right[#right + 1] = preview_node()
  body[#body + 1] = right
  root[#root + 1] = body

  root[#root + 1] = widgets.hints(
    "↑↓ move · enter open · e edit · v view · o open externally · backspace up · r reload · drag edge resize · esc close")
  root[#root + 1] = resize_handle(EXPLORER_CARD_ID, function() return explorer_window end,
    function() return explorer_width end,
    function(value) explorer_width = value end,
    EXPLORER_WIDTH_STATE, explorer_resize)

  scroll_to(TREE_ID, explorer.selected)
  return root
end

-- ---- the editor -----------------------------------------------------------

--- The file, open for editing.
local function open_editor(path)
  local content, err = text.read(path)
  if not content then
    bone.notify("Cannot open " .. path .. ": " .. tostring(err), "error")
    return
  end
  if text.is_binary(content) then
    bone.notify(path .. " is a binary file.", "error")
    return
  end
  local lines, trailing = text.split(content)
  editor = {
    path = path, lines = lines, trailing = trailing, original = content,
    line = 1, col = 0, dirty = false, status = "Loaded", error = nil,
    saving = false, confirm = false, preview = false, md_text = nil, md_nodes = nil,
    undo = {}, redo = {}, undo_kind = nil, undo_row = nil,
  }
  if editor_window and bone.ui.is_open(editor_window) then
    bone.ui.close(editor_window)
  end
  editor_window = bone.ui.popup{
    node = function() return editor_node() end,
    on_key = editor_key,
    width = editor_width, height = 40,
    z = 5,
  }
end

local function close_editor()
  if editor_window and bone.ui.is_open(editor_window) then
    bone.ui.close(editor_window)
  end
  editor_window = nil
end

--- A copy of the buffer, for the undo stack.
local function snapshot()
  local lines = {}
  for i, line in ipairs(editor.lines) do lines[i] = line end
  return { lines = lines, line = editor.line, col = editor.col }
end

--- Remember the buffer as it was, so the edit can be taken back. A run of
--- the same edit (typing a word, or backspacing) is one step.
local function remember(kind, row)
  if kind and editor.undo_kind == kind and editor.undo_row == row then return end
  editor.undo[#editor.undo + 1] = snapshot()
  while #editor.undo > UNDO_MAX do table.remove(editor.undo, 1) end
  editor.redo = {}
  editor.undo_kind = kind
  editor.undo_row = row
end

--- The buffer as it is now, for the redo stack.
local function changed()
  editor.dirty = text.join(editor.lines, editor.trailing) ~= editor.original
end

local function insert(str)
  local row = editor.line
  remember("insert", row)
  local line = editor.lines[row] or ""
  editor.lines[row] = line:sub(1, editor.col) .. str .. line:sub(editor.col + 1)
  editor.col = editor.col + #str
  editor.dirty = true
  editor.status = "Modified"
end

local function split_line()
  local row = editor.line
  remember("split", row)
  local line = editor.lines[row] or ""
  editor.lines[row] = line:sub(1, editor.col)
  table.insert(editor.lines, row + 1, line:sub(editor.col + 1))
  editor.line = row + 1
  editor.col = 0
  editor.dirty = true
  editor.status = "Modified"
end

local function delete_back()
  local row = editor.line
  local line = editor.lines[row] or ""
  if editor.col > 0 then
    remember("delete", row)
    local from = text.prev_offset(line, editor.col)
    editor.lines[row] = line:sub(1, from) .. line:sub(editor.col + 1)
    editor.col = from
    editor.dirty = true
    editor.status = "Modified"
  elseif row > 1 then
    remember("join", row)
    local previous = editor.lines[row - 1]
    editor.lines[row - 1] = previous .. line
    table.remove(editor.lines, row)
    editor.line = row - 1
    editor.col = #previous
    editor.dirty = true
    editor.status = "Modified"
  end
end

local function delete_forward()
  local row = editor.line
  local line = editor.lines[row] or ""
  if editor.col < #line then
    remember("delete", row)
    local to = text.next_offset(line, editor.col)
    editor.lines[row] = line:sub(1, editor.col) .. line:sub(to + 1)
    editor.dirty = true
    editor.status = "Modified"
  elseif row < #editor.lines then
    remember("join", row)
    editor.lines[row] = line .. editor.lines[row + 1]
    table.remove(editor.lines, row + 1)
    editor.dirty = true
    editor.status = "Modified"
  end
end

--- Take the last edit back.
local function undo_edit()
  local step = table.remove(editor.undo)
  if not step then
    editor.status = "Nothing to undo"
    return
  end
  editor.redo[#editor.redo + 1] = snapshot()
  editor.lines = step.lines
  editor.line = step.line
  editor.col = step.col
  editor.undo_kind = nil
  changed()
  editor.status = "Undone"
end

--- Put the last edit back.
local function redo_edit()
  local step = table.remove(editor.redo)
  if not step then
    editor.status = "Nothing to redo"
    return
  end
  editor.undo[#editor.undo + 1] = snapshot()
  editor.lines = step.lines
  editor.line = step.line
  editor.col = step.col
  editor.undo_kind = nil
  changed()
  editor.status = "Redone"
end

local function move_cursor(rows, cols)
  if rows ~= 0 or cols ~= 0 then editor.undo_kind = nil end
  if rows ~= 0 then
    editor.line = math.max(1, math.min(#editor.lines, editor.line + rows))
    editor.col = math.min(editor.col, #(editor.lines[editor.line] or ""))
  end
  if cols ~= 0 then
    local line = editor.lines[editor.line] or ""
    if cols < 0 then
      editor.col = text.prev_offset(line, editor.col)
    else
      editor.col = text.next_offset(line, editor.col)
    end
  end
end

--- Write the file: the buffer goes to the disk as it is.
local function save_editor()
  if editor.saving then return end
  local body = text.join(editor.lines, editor.trailing)
  if body == editor.original then
    editor.dirty = false
    editor.status = "No changes to save"
    refresh_editor()
    return
  end
  local ok, err = bone.fs.write(editor.path, body)
  if ok then
    editor.original = body
    editor.dirty = false
    editor.error = nil
    editor.status = "Saved"
  else
    editor.error = tostring(err or "The file could not be written.")
    editor.status = "Not saved"
  end
  refresh_editor()
end

local function editor_line_node(index)
  local line = editor.lines[index] or ""
  local on = index == editor.line
  local spans = {
    { text = string.format("%s %4d │ ", on and "▸" or " ", index), hl = on and "Accent" or "Dim" },
  }
  if on then
    -- The cursor, where it stands in the line.
    spans[#spans + 1] = { text = line:sub(1, editor.col), hl = "Normal" }
    spans[#spans + 1] = { text = "▏", hl = "Accent" }
    spans[#spans + 1] = { text = line:sub(editor.col + 1), hl = "Normal" }
  else
    spans[#spans + 1] = { text = line, hl = "Normal" }
  end
  return {
    spans = spans,
    font = "mono",
    size = style.size.caption,
    wrap = true,
    selectable = true,
  }
end

--- The buffer as Markdown (built again when the text changed).
local function editor_markdown()
  local body = text.join(editor.lines, editor.trailing)
  if editor.md_text ~= body then
    editor.md_text = body
    editor.md_nodes = markdown.nodes(body, "Normal", { size = style.size.caption })
  end
  return editor.md_nodes
end

editor_node = function()
  local root = { id = EDITOR_CARD_ID, position = "relative", direction = "column", gap = 0,
    grow = 1, max_width = content_width(editor_width), max_height = 720 }

  local header = { direction = "row", align = "center", gap = style.space.md, padding = { 0, 0, 10, 0 } }
  header[#header + 1] = { text = editor.path, bold = true, size = style.size.title,
    grow = 1, wrap = false }
  header[#header + 1] = {
    text = string.format("%s  ·  Ln %d, Col %d%s", editor.dirty and "●" or "",
      editor.line, editor.col + 1, editor.status and ("  ·  " .. editor.status) or ""),
    hl = editor.error and "ErrorMsg" or "Dim",
    size = style.size.small,
  }
  root[#root + 1] = header

  if editor.error then
    root[#root + 1] = { text = editor.error, hl = "ErrorMsg", size = style.size.small,
      padding = { 0, 12 }, selectable = true }
  end

  local body = { direction = "row", gap = 12, grow = 1 }
  body[#body + 1] = {
    id = EDITOR_ID, grow = 1,
    list = {
      count = #editor.lines, estimate = 20,
      key = "files:editor:" .. editor.path,
      item = function(index) return editor_line_node(index) end,
    },
  }
  if editor.preview then
    body[#body + 1] = { width = 1, bg = style.alpha("PromptBorder", "", "fg", "#3a3a3a") }
    local nodes = editor_markdown()
    body[#body + 1] = {
      direction = "column", width = 380, grow = 0,
      id = EDITOR_PREVIEW_ID,
      list = {
        count = #nodes, estimate = 26,
        key = "files:editor:preview:" .. editor.path,
        item = function(index) return nodes[index] end,
      },
    }
  end
  root[#root + 1] = body

  root[#root + 1] = widgets.hints(
    "ctrl+s save · ctrl+z undo · ctrl+y redo · ctrl+p preview · enter newline · tab indent · drag edge resize · esc close")
  root[#root + 1] = resize_handle(EDITOR_CARD_ID, function() return editor_window end,
    function() return editor_width end,
    function(value) editor_width = value end,
    EDITOR_WIDTH_STATE, editor_resize)

  scroll_to(EDITOR_ID, editor.line)
  return root
end

editor_key = function(k)
  if k == "esc" then
    if editor.dirty and not editor.confirm then
      editor.confirm = true
      editor.status = "Unsaved changes: esc again to discard them, ctrl+s to save"
      refresh_editor()
      return true
    end
    close_editor()
    return true
  end
  editor.confirm = false
  if k == "ctrl+s" then
    save_editor()
    return true
  elseif k == "ctrl+z" then
    undo_edit()
  elseif k == "ctrl+y" or k:lower() == "ctrl+shift+z" then
    redo_edit()
  elseif k == "ctrl+p" then
    editor.preview = not editor.preview
  elseif k == "up" then
    move_cursor(-1, 0)
  elseif k == "down" then
    move_cursor(1, 0)
  elseif k == "left" then
    move_cursor(0, -1)
  elseif k == "right" then
    move_cursor(0, 1)
  elseif k == "home" then
    editor.col = 0
    editor.undo_kind = nil
  elseif k == "end" then
    editor.col = #(editor.lines[editor.line] or "")
    editor.undo_kind = nil
  elseif k == "pageup" then
    move_cursor(-10, 0)
  elseif k == "pagedown" then
    move_cursor(10, 0)
  elseif k == "enter" then
    split_line()
  elseif k == "backspace" then
    delete_back()
  elseif k == "delete" then
    delete_forward()
  elseif k == "tab" then
    insert("  ")
  elseif k == "space" then
    insert(" ")
  elseif #k == 1 then
    insert(k)
  else
    return false
  end
  refresh_editor()
  return true
end

-- ---- the commands ---------------------------------------------------------

local function open_explorer(path)
  explorer.root = absolute(path)
  explorer.entries = {}
  explorer.selected = 1
  explorer.preview = nil
  explorer.error = nil
  explorer.note = nil
  load(explorer.root)
  if explorer_window and bone.ui.is_open(explorer_window) then
    bone.ui.update(explorer_window, {})
    return
  end
  explorer_window = bone.ui.popup{
    node = function() return explorer_node() end,
    on_key = function(k) return explorer_key(k) end,
    width = explorer_width, height = 40,
  }
end

explorer_key = function(k)
  if k == "esc" then
    if explorer_window and bone.ui.is_open(explorer_window) then
      bone.ui.close(explorer_window)
    end
    explorer_window = nil
  elseif k == "up" then
    move(-1)
  elseif k == "down" then
    move(1)
  elseif k == "home" then
    explorer.selected = 1
  elseif k == "end" then
    explorer.selected = math.max(#explorer.entries, 1)
  elseif k == "pageup" then
    move(-10)
  elseif k == "pagedown" then
    move(10)
  elseif k == "enter" then
    activate(explorer.selected)
  elseif k == "backspace" then
    go_up()
  elseif k == "e" then
    local entry = explorer.entries[explorer.selected]
    if entry and entry.type == "file" then
      open_editor(join(explorer.root, entry.name))
    end
  elseif k == "v" then
    local entry = explorer.entries[explorer.selected]
    if entry and entry.type == "file" then
      show_preview(entry)
    end
  elseif k == "o" then
    open_externally()
  elseif k == "r" then
    load(explorer.root)
  elseif k == "ctrl+h" then
    explorer.hidden = not explorer.hidden
    load(explorer.root)
  else
    return false
  end
  refresh_explorer()
  return true
end

bone.cmd.create("files", function(c)
  open_explorer(c.args)
end, { desc = "browse the files and open them", aliases = { "explorer" } })

bone.cmd.create("editor", function(c)
  local path = tostring(c.args or ""):match("^%s*(.-)%s*$")
  if path == "" then
    bone.notify("usage: /editor <path>", "error")
    return
  end
  open_editor(absolute(path))
end, { desc = "edit a file", aliases = { "edit" } })
