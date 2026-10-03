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

-- Colors (see runtime/colors/).
bone.colorscheme("black")
