# Using bone

## Screen

Out of the box the screen is a blank slate: the session as plain text and the prompt under it, nothing else. Everything below comes from the style plugin (`cp -r examples/plugins/style ~/.bone/plugins/`) and can be changed or replaced in Lua (see [lua.md](lua.md)).

```text
› your message                         ← the session
  reasoning (dim)
    $ cargo test                       ← a tool call; output collapses to a few rows
    ╰ test result: ok
  The answer, rendered as Markdown.
── ⠙ working 12s  ctrl+c to cancel ──  ← divider (a plain line when idle)
› Message bone…                         ← prompt (grows with its text)
 session title          1.2k in · 40 out │ ~/project   ← statusline
```

Tool calls: `◌` while running, `✕` on failure, nothing when done. Shell calls show `$ command`, a non-zero `exit N`, and their output (first line, `⋮ +N lines`, last lines). Edits show a short `-`/`+` diff; reads and writes are one line.

There are no modes: you always type into the prompt. Type `/` for commands; a list of matches appears as you type.

## Keys

These come from `runtime/tui/defaults.lua`; change them in `~/.bone/tui.lua`.

| Key | Does |
|---|---|
| `enter` | send (or run a `/command`) |
| `alt+enter`, `shift+enter`, `ctrl+j` | new line |
| `ctrl+c` | cancel the running turn; else clear the prompt; else press twice to quit |
| `ctrl+d` | quit (on an empty prompt) |
| `ctrl+r` | pick a session to open |
| `ctrl+n` | new session |
| `up` / `down` | move between prompt lines; past the edges, earlier messages. With `/` suggestions showing, move through them |
| `tab` | complete the selected `/` command |
| `esc` | hide suggestions, or clear a message |
| `pageup` / `pagedown`, `shift+up` / `shift+down`, mouse wheel | scroll the session |
| `ctrl+home` / `ctrl+end` | top / bottom (bottom keeps following new output) |
| mouse drag | highlight text; releasing copies it to the clipboard (through tmux when inside it, else OSC 52, plus `wl-copy`/`xclip` locally). `/set nomouse` hands the mouse back to the terminal |
| `ctrl+left` / `ctrl+right`, `alt+left` / `alt+right` | move by word |
| `ctrl+a` / `ctrl+e`, `home` / `end` | start / end of line |
| `ctrl+w`, `alt+backspace` | delete the word before the cursor |
| `ctrl+u` / `ctrl+k` | delete to the start / end of the line |
| paste | goes into the prompt |

**Panels.** Plugins can dock panels beside, above or below the session (`bone.ui.panel`). Click one (or use the key its plugin gives) to give it the keyboard: the arrows, `pageup`/`pagedown`, `home`/`end` and the wheel scroll it, `tab`/`shift+tab` move to the next/previous panel and back to the prompt, `esc` returns to the prompt. The wheel over a panel always scrolls that panel.

**Approval popup.** By default tool calls run without asking. With the approve plugin installed (`cp -r examples/plugins/approve ~/.bone/plugins/`), a tool that wants to change something asks first: `y` allows, `a` always allows that tool for this session, `n` or `esc` denies, `ctrl+c` cancels the turn. Keys typed in the first 300 ms after it appears are ignored, so text you were typing can't answer it.

**Session picker** (`ctrl+r`, `/sessions`): type to filter, `up`/`down` to move, `enter` to open, `esc` to close.

## Commands

| Command | Does |
|---|---|
| `/help`, `/?` | commands and keys |
| `/help {topic}` | the matching section of these docs in a scrollable window (`/help hooks`, `/help windows`, `/help lua` for a whole file) |
| `/health`, `/checkhealth` | check the setup: provider address and key, sessions folder, terminal, mouse, clipboard route, Lua errors, plus plugins' own checks |
| `/new` | start a new session |
| `/sessions`, `/resume` | pick a session to open |
| `/open {id-prefix}` | open a session by id |
| `/rename {title}` | give this session a title |
| `/fork`, `/fork {N}` | continue in a copy of this session; with N, from before turn N (to try that turn again differently). The original stays as it was |
| `/delete yes` | delete this session and its file (`/delete` alone asks) |
| `/cancel` | cancel the running turn |
| `/quit`, `/exit`, `/q` | quit |
| `/set opt`, `/set noopt`, `/set opt!`, `/set opt=N`, `/set opt?` | options |
| `/colorscheme {name}`, `/theme` | `black` (default), `ansi`, or your own |
| `/hi {Group} fg=… bg=… bold` | change a color |
| `/lua {code}`, `/lua ={expr}` | run Lua / show a value |
| `/source {file}` | run a Lua file |
| `/messages` | recent messages and full Lua errors |
| `/plugin`, `/plugins` | list plugins (TUI and core halves); `/plugin load name`, `/plugin unload name`, `/plugin reload name` act on both halves (picking up edits to their files); `/plugin reload` reloads the core's whole Lua configuration (`core.lua` and core plugins) |
| `/project`, `/project trust`, `/project untrust` | this directory's `.bone/tui.lua`: show it, trust it (runs it now and on later starts here), stop trusting it (unloads it) |

Enter on a partial name runs the highlighted suggestion (`/ses` + enter opens the picker). A message that really starts with `/` can be sent as `//like this`; paths such as `/etc/hosts …` are sent as messages anyway. Commands from Lua or plugins show up in the suggestions too.

## Options

| Option | Default | |
|---|---|---|
| `show_reasoning` | on | show model reasoning |
| `tool_preview_lines` | 4 | rows of tool output under each call |
| `diff_preview_lines` | 8 | rows of diff under each edit |
| `prompt_max_height` | 10 | prompt height limit |

## Command line

```text
bone [-r [ID]] [--connect [PATH]]   TUI (core in-process, or on a socket server)
bone --headless [--listen [PATH]]   core only, on stdio or a socket
bone --init                         starter ~/.bone/core.lua and tui.lua
```

Configuration lives in `~/.bone/` (or `$BONE_CONFIG_DIR`); sessions are saved in `~/.bone/sessions/`. `BONE_BASE_URL`, `BONE_MODEL`, `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, and `BONE_DATA_DIR` override `core.lua` for one run; `BONE_APPROVAL=auto` stops the approve plugin (if installed) from asking.
