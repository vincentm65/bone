-- The / command menu: which commands (or arguments) match what you type,
-- the selection, and what tab, up/down, esc and enter do while it shows.
-- It handles those builtin actions through bone.ui.actions, so keymaps that
-- name "submit", "complete", "dismiss", "up" or "down" keep working; how it
-- looks is bone.ui.suggestions(ctx). Replace this module
-- (~/.bone/runtime/lua/bone/menu.lua) to change how matching works.

local M = {
  max = 8, -- rows at most
  selected = 1, -- of the current matches (it may point past them)
  hidden_for = nil, -- esc hid the menu for exactly this prompt text
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

--- What the menu shows for `text`: { { name, desc } }, and whether they are
--- a command's arguments (completion) rather than command names.
function M.matches(text)
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
        if #out == M.max then
          break
        end
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
  while #unique > M.max do
    table.remove(unique)
  end
  return unique, false
end

--- tab: put the selected match in the prompt.
function M.complete()
  local text = bone.prompt.get()
  local items, argument = M.matches(text)
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
  if #M.matches(text) > 0 then
    M.hidden_for = text
    return true
  end
  return false
end

--- up/down: move through the matches (or let them move in the prompt).
function M.move(by)
  local n = #M.matches(bone.prompt.get())
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
    local item = M.matches(text)[M.selected]
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

-- The menu is a window resting on the prompt, drawn by bone.ui.suggestions
-- whenever there are matches.
M.win = bone.ui.win({
  anchor = "prompt",
  z = -100,
  lines = function(ctx)
    local items = M.matches(bone.prompt.get())
    if #items == 0 or type(bone.ui.suggestions) ~= "function" then
      return {}
    end
    return bone.ui.suggestions({ items = items, selected = M.selected, width = ctx.width, height = ctx.height }) or {}
  end,
})

return M
