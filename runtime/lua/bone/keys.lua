-- The standard key bindings, for every frontend: the prompt (the "main"
-- context), a focused window ("popup") and a focused panel ("panel"). They
-- name built-in actions, slash commands and Lua functions, so they mean
-- the same in the TUI and the GUI (where egui's text box does the editing
-- actions itself). Change them from ui.lua with bone.keymap.set.

-- Enter during a turn queues the message in the core: "steer" joins the
-- running turn at its next step, "next" waits for a turn of its own. The
-- actions queue_steer and queue_next do one or the other whatever this says.
-- (A library copy made before this moved still defines it itself.)
if not pcall(bone.o.info, "queue_mode") then
  bone.o.define("queue_mode", "steer", {
    desc = "what enter does during a turn: steer or next",
    choices = { "steer", "next" },
  })
end

local function map(keys, context)
  for key, action in pairs(keys) do
    bone.keymap.set(key, action, { context = context })
  end
end

-- The prompt (the "main" context).
map({
  ["f1"] = "/help",
  ["enter"] = "submit",
  ["ctrl+v"] = "paste_clipboard",
  ["super+v"] = "paste_clipboard",
  -- Windows terminals keep ctrl+v for their own paste and send nothing
  -- when the clipboard holds only an image.
  ["alt+v"] = "paste_clipboard",
  ["alt+enter"] = "newline",
  ["shift+enter"] = "newline",
  ["ctrl+j"] = "newline",
  ["tab"] = "complete",
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["ctrl+d"] = "quit_if_empty",
  ["ctrl+o"] = "sessions",
  ["ctrl+n"] = "new_session",
  ["ctrl+p"] = "focus_next",
  ["ctrl+r"] = function()
    -- Toggle the model reasoning (live, in the chat, and in the transcript).
    -- The same switch as bone.o.apply("show_reasoning!"); it redraws on its own.
    bone.o.apply("show_reasoning!")
    bone.settings.set(bone.api_info().side .. ".show_reasoning", bone.o.show_reasoning)
  end,

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

-- While a panel has the keyboard (after the panel's own keys). Scrolling and
-- dismiss act on the panel; text that is not mapped is ignored.
map({
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["tab"] = "focus_next",
  ["shift+tab"] = "focus_prev",
  ["up"] = "up",
  ["down"] = "down",
  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  ["home"] = "scroll_top",
  ["end"] = "scroll_bottom",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
}, "panel")
