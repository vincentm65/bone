# Customizing the TUI's look and behavior

Where: `~/.bone/tui.lua` (global) or `.bone/tui.lua` in a project (loaded only
after `/plugins trust`). A Lua error never crashes bone; it is shown once and
listed by `/health`. The TUI reloads when a file changes, so you can edit and
watch. With nothing defined, Rust draws a plain-text chat and a bare prompt;
everything else is Lua.

Reference: `docs/lua.md` sections "TUI (`tui.lua`)" and "Drawing the screen".
Source: `crates/bone-tui/src/` (`lua.rs` the API, `ui.rs` the drawing model,
`render.rs` the layout, `keymap.rs` keys, `theme.rs` colors); the defaults are
`runtime/tui/defaults.lua` and `runtime/lua/bone/ui/`.

## Options

Fixed options are assigned directly; the user can also change them with
`/config` (saved in `settings.json`, which your `tui.lua` overrides anyway if
you assign the name):

```lua
bone.o.show_reasoning = false      -- hide model reasoning
bone.o.tool_detail = "rows"        -- tool calls: "summary" (default), "rows", "full" (ctrl+t)
bone.o.tool_preview_lines = 4      -- rows of tool output under each call
bone.o.diff_preview_lines = 8      -- rows of diff under each edit
bone.o.prompt_max_height = 10
bone.o.mouse = false               -- leave the mouse to the terminal
bone.o.autoreload = false          -- stop reloading Lua on file changes
```

Plugins add options with `bone.o.define`; users change them the same way:

```lua
bone.o.define("review_limit", 10, {
  type = "integer", desc = "Lines to show in review",
  choices = nil,
  on_change = function(new, old) bone.notify("limit: " .. old .. " -> " .. new) end,
})
bone.o.get("review_limit")         -- read
bone.o.info("review_limit")        -- { type, value, default, desc, choices? }
```

`bone.o.apply("name=value")` does the same from a string.

## Keys

Every key comes from Lua. `bone.keymap.set(key, value, opts)`:

```lua
bone.keymap.set("f2", "/sessions")                            -- run a slash command
bone.keymap.set("ctrl+l", "new_session")                      -- run a builtin action
bone.keymap.set("alt+s", function() bone.cmd("set noshow_reasoning") end)
bone.keymap.set("ctrl+q", "interrupt", { context = "popup" }) -- only while a popup is open
bone.keymap.set("g g", "/health")                             -- key sequences
bone.keymap.del("ctrl+n")
```

A Lua callback consumes the key unless it returns `false`. Key names: `ctrl+`,
`alt+`, `shift+` with `enter`, `esc`, `tab`, `backspace`, `delete`, arrows,
`home`, `end`, `pageup`, `pagedown`, `space`, `f1`–`f24`, `wheelup`,
`wheeldown` or a character.

Named contexts are separate keymaps that you focus:

```lua
bone.keymap.context("review", { fallback = { "main" }, priority = 10 })
bone.keymap.set("r", function() ... end, { context = "review" })
bone.keymap.focus("review")    -- this context's keys apply until cleared
bone.keymap.clear()            -- back to main
```

Precedence: a focused popup, then a focused panel, then a focused named
context, then `main`. `bone.keymap.raw(fn)` intercepts every key before
mapping (return `true` to consume); `bone.keymap.raw_del(id)`.

Builtin actions (the strings you can map): `submit queue_steer queue_next
newline left right up down word_left word_right line_start line_end backspace
delete delete_word delete_to_start delete_to_end scroll_up scroll_down page_up
page_down scroll_top scroll_bottom complete dismiss interrupt quit quit_if_empty
new_session sessions focus_next focus_prev focus_prompt`. Lua can take over any
of them: `bone.ui.actions["submit"] = function() ... end` (return `true` if
handled; inside your handler the action runs the built-in).

## Slash commands

```lua
bone.cmd.create("hello", function(c)        -- c.args raw string, c.argv split
  bone.notify("hi " .. c.args)
end, { desc = "say hi", aliases = { "hey" } })
bone.cmd.del("hello")
bone.cmd("sessions")                        -- run any command from Lua
```

With typed arguments and completion:

```lua
bone.cmd.create("greet", function(c)
  bone.notify(c.arguments.word)             -- typed when opts.args is given
end, {
  desc = "greet someone",
  args = {
    { name = "count", type = "integer", default = 1 },
    { name = "word", type = "string", required = true },
    { name = "rest", type = "string", variadic = true },  -- must be final
  },
  complete = function(ctx)                    -- ctx = { command, text, args, token, argv }
    return { { value = "world", desc = "the default" }, "there" }
  end,
})
```

`bone.cmd.create` also replaces built-in commands (`/help`, `/new`, …; they
are defined in `runtime/lua/bone/commands.lua`), and `bone.cmd.del` removes
them.

## Colors

```lua
bone.colorscheme("ansi")                                  -- "black" (default) or "ansi"
bone.hl.set("MyGroup", { fg = "#7dcfff", bold = true })   -- fg/bg: #rrggbb, a name, or 0-255
bone.hl.set("Link2", { link = "ToolError", underline = true })
bone.hl.get("MyGroup")
bone.hl.names()
```

Named groups used by the standard UI: `Normal Dim Accent UserPrompt
UserMessage Reasoning ToolName ToolArgs ToolPath ToolSummary ToolOutput
ToolGutter ToolRunning ToolError DiffAdd DiffDelete ShellProgram ShellPath
ShellFlag ShellString ShellVariable ShellComment ShellOperator MdHeading
MdBold MdItalic MdCode MdCodeBlock MdQuote MdBullet MdLink Notice ErrorMsg
WarningMsg WinSeparator StatusLine StatusLineDim Selection Placeholder
PopupBorder PopupTitle PanelTitle PanelTitleFocus`. Colorschemes are
`colors/<name>.lua` in `~/.bone/` or a plugin.

## Layout and regions

The screen's rows come from `bone.ui.layout`, a tree read top to bottom:

```lua
bone.ui.layout = { "top", { cols = { "left", "chat", "right" }, sep = "│" },
                   "divider", "above_prompt", "prompt", "statusline" }  -- the default
```

An entry is a name, `{ "name", size = … }`, or a split: `{ rows = { … } }`
stacks, `{ cols = { … }, sep = "│" }` goes side by side; splits nest. `size`
is cells, `"30%"`, `"auto"` (natural size) or `"fill"`. `chat`, `prompt`,
`divider`, `statusline` and `message` are built in; leave one out and it is
not shown.

Anything else in the layout is a region you define:

```lua
bone.ui.regions.mybar = { size = 1, render = function(ctx) return { "hello" } end }
bone.ui.regions.right = { size = 30, render = function(ctx) ... end }
-- render(ctx) gets { region, width, height, spinner, popup, session } and
-- returns lines, or nil to hide. Regions are redrawn every frame: keep them light.
```

`size` is a number, `"auto"` (fit content up to `max`, default 10) or a bare
function means `"auto"`. Row regions size in rows. `bone.ui.title(ctx)` sets
the terminal title; `bone.ui.spinner = { frames = { "◐", "◓" }, interval = 120 }`
changes the spinner shown in every `ctx.spinner`.

`bone.ui.clear()` removes every view, region, action and UI function — a
config that starts from a blank screen.

## The prompt box

```lua
bone.ui.prompt = {
  prefix = { { "› ", "UserPrompt" } },                 -- lines (or fn(ctx)); later rows indent to match
  placeholder = { { "Message bone…", "Placeholder" } },
}
```

Every field is optional: `prefix`, `placeholder`, `continuation` (line before
wrapped rows), `border` (`"rounded"`, `"single"`, `"double"`, `"thick"`,
`"ascii"`, `true`, or `{ style, chars, hl, sides = "tblr" }`), `background`
(a group filling the box), `padding` (columns, or `{ rows, cols }`), `min` /
`max` (text rows; default 1 and `bone.o.prompt_max_height`), `top` / `bottom`
(a line or fn(ctx) drawn into that border edge).

The functions get `ctx = { width, text, lines, empty, cursor = { row, col },
selection, focused, running, elapsed, spinner, session }`; `top`/`bottom`
additionally get `rows`, `height`, `text_width`, `scroll`, `cursor.screen`.

Inline text styling while the user types:

```lua
bone.ui.prompt_highlight = function(ctx)   -- runs every frame while the prompt has text
  local s, e = ctx.text:find("^/%w+")
  return { highlights = s and { { row = 0, from = s - 1, to = e, hl = "Accent" } } or {} }
end
```

Each highlight colors chars `from` to `to` (exclusive, from 0) of line `row`;
`ghost` adds text after the cursor at line end (in `ghost_hl`, default
`Placeholder`).

## Statusline and divider

```lua
bone.ui.statusline = function(ctx)
  return { { " " .. ctx.title, "StatusLine" }, "%=", { ctx.spinner, "Accent" }, " " }
end
bone.ui.divider = function(ctx)   -- the line between the chat and the prompt
  return { { fill = "─", hl = "WinSeparator" } }
end
```

Each returns one line (`"%="` is a blank stretch); the row exists only while
the function is defined. The statusline `ctx` has `title`, `popup` (the active
keymap context unless `main`), `panel` (focused panel id), `jobs` (running
jobs), `spinner`, `width` and `session` (`{ title, cwd, running, elapsed }`
or nil).

## A line is a list of items

Everywhere above: a line is a list of items — a plain string,
`{ "text", "Group" }`, or `{ fill = "─", hl = "Group" }` (stretches to fill
the row). An empty table is a blank line; long lines are cut with `…`.
Helpers: `bone.text.wrap(spans, width, { first, rest, pad })`,
`bone.text.clip`, `bone.text.truncate`, `bone.text.width`,
`bone.text.shell(cmd)` (highlighted command spans), `bone.strwidth(s)`.

## Overriding the runtime

The default look is `runtime/lua/bone/ui/` (views, statusline, layout,
tray, …) set up by `runtime/tui/defaults.lua`. You can replace any
runtime file by writing the same path under `~/.bone/runtime/` — start from
the built-in one:

```lua
-- ~/.bone/runtime/lua/bone/ui/layout.lua
local M = bone.builtin("bone.ui.layout")   -- the built-in module, not this file
local setup = M.setup
function M.setup(opts)
  setup(opts)
  bone.ui.regions.top = function() return { "my header" } end
end
return M
```

`/runtime` lists overrides; `/runtime reset FILE` (or `all`) moves your copy
to `~/.bone/runtime-backup/` and restores the built-in.

## Worked example: a compact top bar

```lua
bone.ui.layout = { "top", "chat", "divider", "prompt", "statusline" }
bone.ui.regions.top = {
  size = 1,
  render = function(ctx)
    local s = ctx.session
    return { { " " .. (s and s.title or "new"), "StatusLineDim" },
             "%=", { (s and s.running) and (ctx.spinner .. " on") or "idle", "Dim" } }
  end,
}
```