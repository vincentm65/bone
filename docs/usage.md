# Using bone

## Screen

Out of the box the screen is the runtime's standard UI (`runtime/lua/bone/ui/`). It can be changed or replaced in Lua (see [lua.md](lua.md)).

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
| `enter` | send (or run a `/command`). While a turn runs, the message is queued: by default it joins that turn at its next step; with `bone.o.queue_mode = "next"` in `tui.lua` it waits for a turn of its own. Queued messages show at the end of the chat |
| `up` on an empty prompt | edit the last queued message: `enter` saves it in place, `esc` leaves it as it was |
| `alt+enter`, `shift+enter`, `ctrl+j` | new line |
| `ctrl+c` | cancel the running turn; else clear the prompt; else press twice to quit |
| `ctrl+d` | quit (on an empty prompt) |
| `ctrl+o` | pick a session to open |
| `f1` | open the searchable help popup: Commands, Keys and Docs |
| `down` / `ctrl+b` | the tray below the prompt: queued messages, sub-agents and shell jobs. `down` on an empty prompt moves into it (the Queue page first when anything is queued; `up` from its top row leaves), `tab` switches pages, `enter` opens one (a sub-agent's session in the chat, a shell job as a terminal in the tray), `c` cancels, `esc` goes back; a click does the same. On the Queue page `enter` edits a message, `s` switches it between steer and next (when no turn runs: sends it now), `shift+up`/`shift+down` move it, `d` drops it and `r` resumes a paused queue. `ctrl+b` folds the tray to one line. In a sub-agent's session, `‹ main` in the tray or `esc` on an empty prompt goes back |
| `ctrl+r` | show / hide the model reasoning (live in the chat and in the transcript) |
| `ctrl+t` | how much of each tool call to show: a summary line per stretch of calls ("Read 3 files, ran 2 shell commands"; edits and failures in full) → a row per call → everything in full → back. It stays until you press it again |
| `ctrl+n` | new session |
| `up` / `down` | move between prompt lines; past the edges, earlier messages. With `/` suggestions showing, move through them |
| `tab` | complete the selected `/` command |
| `esc` | hide suggestions, or clear a message |
| `pageup` / `pagedown`, `shift+up` / `shift+down`, mouse wheel | scroll the session |
| `ctrl+home` / `ctrl+end` | top / bottom (bottom keeps following new output) |
| mouse drag | highlight text; releasing copies it to the clipboard (through tmux when inside it, else OSC 52, plus `wl-copy`/`xclip` locally). `bone.o.mouse = false` in `tui.lua` hands the mouse back to the terminal |
| `ctrl+left` / `ctrl+right`, `alt+left` / `alt+right` | move by word |
| `ctrl+a` / `ctrl+e`, `home` / `end` | start / end of line |
| `ctrl+w`, `alt+backspace` | delete the word before the cursor |
| `ctrl+u` / `ctrl+k` | delete to the start / end of the line |
| `ctrl+v` / `super+v` | read the local clipboard: attach a screenshot, or paste text; `super+v` is Command+V on macOS when the terminal forwards it |
| terminal paste | goes into the prompt as text |

**Panels.** Plugins can dock panels beside, above or below the session (`bone.ui.panel`). Click one (or use the key its plugin gives) to give it the keyboard: the arrows, `pageup`/`pagedown`, `home`/`end` and the wheel scroll it, `tab`/`shift+tab` move to the next/previous panel and back to the prompt, `esc` returns to the prompt. The wheel over a panel always scrolls that panel.

**Approval popup.** By default tool calls run without asking. Custom hooks can pause with `bone.ask`; a TUI handler displays the question and replies with `ask/respond` (see [lua.md](lua.md#asking-the-user)).

**Agents sidebar** (`ctrl+o`, `/sessions`): a docked left column groups top-level conversations into **Running** (animated spinners), **Recent** (active or opened within the last hour by default; configure `bone.ui.sessions_recent_seconds` in `tui.lua`), and **History**, newest activity first within each group. It shows titles, last-activity times, total input + output tokens, and user-turn counts. Type to filter by title or directory, `up`/`down` to select, and `enter` or a mouse click to open. First open selects the current chat. Switching leaves the sidebar open and returns keyboard focus to the prompt; background turns keep running. `ctrl+o` focuses it again, or closes it when already focused; `esc` returns to the prompt without closing it. Page keys and `home`/`end` navigate longer lists; the mouse wheel scrolls. Running status updates live, including turns already active when connecting to the core; usage totals refresh when turns finish. `tab` switches to the **Processes** page, remembering selection and scroll on each tab: every chat's background shells and running sub-agents, grouped by chat, whichever chat is on screen. `enter` opens the chat a shell belongs to, or the sub-agent's own chat, and `x` stops the selected one. The tray below the prompt only shows the on-screen chat's shells and sub-agents.

## Images

Copy a screenshot to your system clipboard, focus the prompt and press `ctrl+v`. Bone reads the image in the background and shows its name and dimensions below the composer. Add a question or press `enter` to send the image alone. `/paste` does the same read when the terminal intercepts your paste shortcut; on macOS, forwarded Command+V also works. Ordinary clipboard text still pastes as text.

`/attach PATH` adds an image file (paths may contain spaces or be quoted). `/detach 2` removes the second image; `/detach all` clears the attachment list. `esc` or `ctrl+c` clears the whole draft. Each open session keeps its draft, including images; queue editing and prompt history keep images too. Enter waits until pending image reads finish, so a screenshot cannot accidentally miss its message.

Native clipboard access runs on the TUI's machine, including when connected to a separate core. Linux uses X11 or a Wayland compositor with clipboard data-control support (or its XWayland bridge). A terminal's usual paste shortcut may be handled by the terminal before Bone sees it; use `/paste` or configure the terminal to forward `ctrl+v`. In SSH or a desktop-free environment, use `/attach` on a local file. Native Windows builds use the Windows clipboard; a Linux build in WSL uses its Linux desktop clipboard.

PNG, JPEG and WebP inputs become lossless PNG attachments. Limits: 8 images per message, 20 MiB per input and resulting PNG, 40 megapixels per image, and 40 MiB of image data per message. Select a model that accepts images and enable vision in its server configuration (for example, TabbyAPI can load a vision-capable model with its vision component disabled); `supports_images = false` in a provider entry rejects attachments before submission and preserves the draft. Leave it unset for the endpoint to decide, or set it to `true` for a vision provider.

Saved sessions and forks refer to durable files under `<data_dir>/attachments/<sha256>.png`; pixels are sent to the model only when needed. Attachment files are retained, including uploads from drafts you discard; automatic garbage collection is not implemented. Back up the attachments directory together with sessions when moving your history.

The model can also use `read_file` to view PNG, JPEG and WebP files on the core machine. Images are recognized by their contents and returned as pixels with the same limits and storage as attachments. Line ranges apply only to text files. Invalid images or a model with `supports_images = false` produce a tool error, allowing the model to continue.

## Commands

| Command | Does |
|---|---|
| `/help`, `/?` | searchable help popup with Commands, Keys and Docs tabs (also `f1`) |
| `/help {topic}` | the matching section of these docs in a scrollable window (`/help hooks`, `/help windows`, `/help lua` for a whole file) |
| `/md [relative path]` | browse the working directory's Markdown files in a large popup, or open a listed document directly |
| `/health`, `/checkhealth` | check the setup: provider address and key, sessions folder, terminal, mouse, clipboard route, Lua errors, plus plugins' own checks |
| `/setup` | add a model provider: kind, URL, model and key (kept in `~/.bone/secrets.json`), and catalog plugins; opens by itself on a first run with no provider |
| `/catalog` | browse the plugin catalog: install, update and remove packages (each file checked against the catalog's hashes) |
| `/config`, `/settings` | the settings page: TUI options, which provider and model to use, plugins on and off, and plugins' own settings; everything is saved in `~/.bone/settings.json` |
| `/paste` | paste a screenshot or text from the local clipboard |
| `/attach PATH` | attach a PNG, JPEG or WebP file |
| `/detach [N|all]` | remove an attachment (default: the last one) |
| `/model [name]` | show the current model and provider, or set the model for the current provider |
| `/runtime`, `/runtime reset FILE`, `/runtime reset all` | your copies of built-in Lua files in `~/.bone/runtime/` (bone never overwrites them); reset goes back to the built-in ones, keeping your copies in `~/.bone/runtime-backup/` |
| `/new`, `/clear` | start a new session |
| `/sessions`, `/resume` | pick a session to open |
| `/rename {title}` | give this session a title |
| `/fork`, `/fork {N}` | continue in a copy of this session; with N, from before turn N (to try that turn again differently). The original stays as it was |
| `/compact`, `/compact clear` | summarize the older part of this session for the model (the latest turns stay word for word); the chat and the session file keep everything, and one line notes the tokens saved. `clear` sends the whole history again. Bone also compacts by itself when the model says the context is too long, or before a call over `compact.limit` (`/config` → Compaction) |
| `/queue`, `/queue clear`, `/queue resume` | list the queued messages, empty the queue, let a paused queue go on (after a cancel, restart or queued-message error) |
| `/quit`, `/exit`, `/q` | quit |
| `/plugins` | open the plugin settings; `/plugins list` lists plugins (TUI and core halves); `/plugins load name`, `/plugins unload name`, `/plugins reload name` act on both halves (picking up edits to their files); `/plugins reload` reloads the core's whole Lua configuration (`core.lua` and core plugins) |

`/rename`, `/fork`, `/compact` and `/plugins` are Lua commands from `runtime/tui/defaults.lua`: change or remove them like any other (`bone.cmd.del("fork")`).

Enter on a partial name runs the highlighted suggestion (`/ses` + enter opens the picker). A message that really starts with `/` can be sent as `//like this`; paths such as `/etc/hosts …` are sent as messages anyway. Commands from Lua or plugins show up in the suggestions too.

**Help popup.** Type to filter the current tab; `tab` / `shift+tab` or `left` / `right` switch between Commands, Keys and Docs. Search matches command aliases and descriptions too, and includes commands registered by plugins. Use `up` / `down`, the wheel, `pageup` / `pagedown` or `home` / `end` to navigate; `ctrl+u` clears the search. `enter` on a command puts it in the prompt ready to edit and submit; on a shortcut or doc topic, it opens the guide in a scrollable window. Close the guide with `esc` to return to the same search. `esc` closes help and returns to your draft. The Keys tab describes the default mappings; your `tui.lua` can override them.

## Markdown popup

`/md` recursively lists `.md` files (case-insensitive) in the current session's working directory, including hidden files but excluding `.git` internals and symlinks. On a new chat it uses Bone's launch directory. The popup leaves your draft and chat untouched; files are read locally, not sent to the model. `/md docs/usage.md` selects that relative path after discovery.

Type in the file pane to filter relative paths (all search words must match); `backspace` edits and `ctrl+u` clears the filter. `up` / `down`, the wheel, page keys and `home` / `end` select files and preview them. `enter` focuses the reader; `tab` / `shift+tab` switch panes. In the reader those navigation keys scroll, and `space` pages down. `ctrl+r` rescans and reloads the selected file; `esc` closes. Reading positions are remembered while the popup stays open. Narrow terminals show only the active pane.

The preview uses Bone's themed Markdown renderer for headings, emphasis, links, lists, quotes, code blocks and wrapping tables. Links are displayed, not executed. Discovery and reads run as background jobs using `find` and `head` from your PATH; no new Rust APIs or model calls are involved. UTF-8 filenames are supported, including spaces and newlines. Scans stop after 30 seconds or 20,000 files and report incomplete results; each read has a 10-second timeout and a 1 MiB limit. Unreadable, empty and oversized documents have explicit messages. This is a browser for trusted local projects, not a filesystem sandbox against concurrent symlink replacement.

Implementation: `require("bone.help").markdown(path)` in the existing embedded `runtime/lua/bone/help.lua`. Lua regression tests: `luajit tests/md.lua`.

## Options

| Option | Default | |
|---|---|---|
| `show_reasoning` | off | show model reasoning (`ctrl+r`) |
| `tool_detail` | summary | tool calls: `summary`, `rows` or `full` (`ctrl+t`) |
| `tool_preview_lines` | 4 | rows of tool output under each call |
| `diff_preview_lines` | 8 | rows of diff under each edit |
| `prompt_max_height` | 10 | prompt height limit |

Options are set in `~/.bone/tui.lua` (`bone.o.tool_detail = "rows"`). What you choose with `ctrl+t` and `ctrl+r` is remembered in `~/.bone/settings.json`, which bone writes for you; `tui.lua` still wins when it sets the same option. The same file remembers which provider to use and your changes to providers (`"provider"`, `"providers": { "<name>": { "model": … } }`): `/config` edits every provider's settings there, over what `core.lua` gives them, and adds new ones.

## Command line

```text
bone [-r [ID]] [--connect [PATH]]   TUI (core in-process, or on a socket server)
bone --headless [--listen [PATH]]   core only, on stdio or a local socket/named pipe
bone --init                         starter ~/.bone/core.lua and tui.lua
```

Configuration lives in `~/.bone/` (or `$BONE_CONFIG_DIR`); sessions are saved in `~/.bone/sessions/`. `bone --import-bone [DB]` brings in the first bone's conversations (default `~/.bone-rust/data/conversations.db`) as sessions, with their usage and latest checkpoint; run it again to pick up newer ones, and sessions you continued here are never overwritten. `BONE_BASE_URL`, `BONE_MODEL`, `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, and `BONE_DATA_DIR` override `core.lua` for one run.

On Windows, `--listen` and `--connect` use `\\.\pipe\bone3-<USERNAME>` by default (or the named-pipe path you provide). Only the current user can connect. Shell commands run in `cmd.exe`, and background jobs use ConPTY.
