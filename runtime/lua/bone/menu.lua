-- The / command menu: which commands (or arguments) match what you type,
-- the selection, and what tab, up/down, the wheel, esc and enter do while
-- it shows. The window shows as many matches as fit and scrolls through the
-- rest (wheel, shift+up/down, pageup/pagedown).
-- It handles those builtin actions through bone.ui.actions, so keymaps that
-- name "submit", "complete", "dismiss", "up" or "down" keep working; how it
-- looks is bone.ui.suggestions(ctx). Replace this module
-- (~/.bone/runtime/lua/bone/menu.lua) to change how matching works.

local M = {
  offset = 0, -- first match row shown (the window scrolls when it overflows)
  selected = 1, -- of the current matches (it may point past them)
  hidden_for = nil, -- esc hid the menu for exactly this prompt text
  last_text = nil, -- the prompt text the current scroll is for
  last_selected = 1, -- to tell a moved selection from a scrolled window
}

-- A popup or panel with the keyboard has no menu.
local function blocked()
  return bone.keymap.current() == "popup" or bone.ui.panel.focused() ~= nil
end

-- Words, as the command line splits them.
local function words(s)
  local out = {}
  for w in s:gmatch("%S+") do
    out[#out + 1] = w
  end
  return out
end

--- All of the matches for `text`: { { name, desc } }, and whether they are
--- a command's arguments (completion) rather than command names. No cap:
--- the window shows what fits and scrolls through the rest.
function M.all(text)
  if blocked() or text:sub(1, 1) ~= "/" then
    return {}, false
  end
  local rest = text:sub(2)
  if rest:sub(1, 1) == "/" or M.hidden_for == text then
    return {}, false
  end
  local ws = rest:find("%s")
  if ws then
    -- "/name args": the command's own completion, for the last word.
    local word = rest:sub(1, ws - 1)
    local args = rest:sub(ws + 1):gsub("^%s+", "")
    local command = bone.cmd.find(word)
    if not command then
      return {}, false
    end
    local token = args:match("(%S*)$")
    local got = bone.cmd.complete(command, {
      command = command,
      text = text,
      args = args,
      token = token,
      argv = words(args),
    })
    if type(got) ~= "table" then
      return {}, false
    end
    local out = {}
    for _, item in ipairs(got) do
      local name, desc
      if type(item) == "string" then
        name, desc = item, ""
      elseif type(item) == "table" then
        name, desc = item.value or item.name, item.desc or ""
      end
      if name and name:sub(1, #token) == token then
        out[#out + 1] = { name = name, desc = desc }
      end
    end
    return out, true
  end
  -- "/partial": command names and aliases starting with it.
  local out = {}
  for _, c in ipairs(bone.cmd.list()) do
    for _, name in ipairs({ c.name, unpack(c.aliases or {}) }) do
      if name:sub(1, #rest) == rest then
        out[#out + 1] = { name = name, desc = c.desc or "" }
      end
    end
  end
  table.sort(out, function(a, b)
    if a.name ~= b.name then
      return a.name < b.name
    end
    return a.desc < b.desc
  end)
  local unique = {}
  for _, it in ipairs(out) do
    if #unique == 0 or unique[#unique].name ~= it.name then
      unique[#unique + 1] = it
    end
  end
  return unique, false
end

--- tab: put the selected match in the prompt.
function M.complete()
  local text = bone.prompt.get()
  local items, argument = M.all(text)
  local item = items[M.selected]
  if item then
    if argument then
      local split = 0
      for i = #text, 1, -1 do
        if text:sub(i, i):match("%s") then
          split = i
          break
        end
      end
      bone.prompt.set(text:sub(1, split) .. item.name .. " ")
    else
      bone.prompt.set("/" .. item.name .. " ")
    end
  end
  return true
end

--- esc: hide the menu until the text changes (or let esc do its usual).
function M.dismiss()
  local text = bone.prompt.get()
  if #M.all(text) > 0 then
    M.hidden_for = text
    return true
  end
  return false
end

--- up/down: move through the matches (or let them move in the prompt).
function M.move(by)
  local n = #M.all(bone.prompt.get())
  if n == 0 then
    return false
  end
  M.selected = (M.selected - 1 + by) % n + 1
  return true
end

--- enter: run a /command (the selected match for a partial name), or let
--- the text be sent. "//text" sends "/text"; "/etc/hosts …" is a message.
function M.submit()
  local text = bone.prompt.get()
  local rest = text:gsub("^%s+", ""):match("^/(.*)$")
  if not rest then
    return false
  end
  if rest:sub(1, 1) == "/" then
    bone.prompt.set((text:gsub("//", "/", 1)))
    return false
  end
  local word = rest:match("^%S*")
  if word:find("/", 1, true) then
    return false
  end
  local line = rest
  if not bone.cmd.find(word) then
    local item = M.all(text)[M.selected]
    if not item then
      bone.notify("Unknown command /" .. word .. " (start with // to send it as a message)", "error")
      return true
    end
    line = item.name .. rest:sub(#word + 1)
  end
  bone.prompt.history_add(text)
  bone.prompt.set("")
  M.selected = 1
  bone.cmd(line)
  return true
end

bone.ui.actions.submit = M.submit
bone.ui.actions.complete = M.complete
bone.ui.actions.dismiss = M.dismiss
bone.ui.actions.up = function()
  return M.move(-1)
end
bone.ui.actions.down = function()
  return M.move(1)
end

--- The wheel, shift+up/down and pageup/pagedown move the selection through
--- the whole list (the window follows it), when the menu overflows
--- (returning false lets the chat scroll instead).
local function scroll_by(by)
  local n = #M.all(bone.prompt.get())
  if n == 0 then
    return false
  end
  M.selected = math.max(1, math.min(M.selected + by, n))
  return true
end
bone.ui.actions.scroll_up = function()
  return scroll_by(-3)
end
bone.ui.actions.scroll_down = function()
  return scroll_by(3)
end
bone.ui.actions.page_up = function()
  return scroll_by(-8)
end
bone.ui.actions.page_down = function()
  return scroll_by(8)
end

-- The menu is a window resting on the prompt, drawn by bone.ui.suggestions
-- whenever there are matches.
M.win = bone.ui.win({
  anchor = "prompt",
  row = -1,
  col = 0,
  z = -100,
  -- No fixed height: the window fits what bone.ui.suggestions returns (at
  -- most eight rows, the footer and the border), so it rests on the prompt
  -- however few matches there are.
  lines = function(ctx)
    local items = M.all(bone.prompt.get())
    if #items == 0 or type(bone.ui.suggestions) ~= "function" then
      return {}
    end
    -- Rows the box can show: its border takes two, the footer one more.
    local height = 8
    if M.last_text ~= bone.prompt.get() then
      M.last_text = bone.prompt.get()
      M.offset = 0
      M.selected = 1
      M.last_selected = 1
    end
    local n = #items
    M.selected = math.max(1, math.min(M.selected, n))
    -- The window follows the selection, but wheel-scrolling is not pulled
    -- back to it.
    if M.selected ~= M.last_selected then
      M.last_selected = M.selected
      if M.selected < M.offset + 1 then
        M.offset = M.selected - 1
      elseif M.selected > M.offset + height then
        M.offset = M.selected - height
      end
    end
    M.offset = math.max(0, math.min(M.offset, math.max(n - height, 0)))
    local first = M.offset + 1
    local shown = {}
    for i = first, math.min(n, first + height - 1) do
      shown[#shown + 1] = items[i]
    end
    local footer
    if n > #shown then
      local where = ""
      if M.offset > 0 then
        where = where .. "↑"
      end
      if M.offset + #shown < n then
        where = where .. "↓"
      end
      footer = where .. " " .. first .. "–" .. (M.offset + #shown) .. " of " .. n
    end
    return bone.ui.suggestions({
      items = shown,
      selected = M.selected - M.offset,
      width = ctx.width,
      height = ctx.height,
      footer = footer,
    }) or {}
  end,
})

return M
