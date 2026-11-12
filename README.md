<p align="center">
  <img src="docs/images/logo.png" width="96" alt="">
</p>
<h1 align="center">bone</h1>
<p align="center">
  A coding agent for your terminal and your desktop.<br>
  A small Rust core, any OpenAI-compatible model, and Lua for everything you'd want to change.
</p>
<p align="center">
  <a href="https://www.npmjs.com/package/bone-agent"><img src="https://img.shields.io/npm/v/bone-agent?color=7aa2f7&label=npm" alt="npm version"></a>
  <img src="https://img.shields.io/badge/platforms-Linux%20%C2%B7%20macOS%20%C2%B7%20Windows-7aa2f7" alt="Linux, macOS and Windows">
  <img src="https://img.shields.io/npm/l/bone-agent?color=7aa2f7" alt="MIT license">
</p>

![bone in a terminal: asked to fix failing tests, it runs them, reads the code, edits one file and runs the tests again](docs/images/tui.png)

You describe a change. bone reads the code, runs commands, edits files and
tells you what it did. It works with a model on your own machine or a hosted
one. It is a single binary with nothing else to install and no account to
create.

```sh
npm install -g bone-agent
cd your/project
bone
```

The first run opens a short setup screen where you pick a local server or a
hosted provider. After that, type a message and press enter.

## What you get

- **Your model.** Any OpenAI-compatible `/chat/completions` endpoint works: llama.cpp, vLLM, tabbyAPI, OpenRouter, DeepSeek, OpenAI. Plugins add providers that are not OpenAI-shaped, and you can write your own in Lua.
- **A terminal UI and a desktop app.** Both are clients of the same core, so a chat open in one is live in the other.
- **Lua for everything.** Providers, tools, hooks, keys, commands, colors, the statusline and how each tool call is drawn are Lua files in `~/.bone`. The standard UI is Lua too, and you can replace it piece by piece.
- **Plugins.** `/catalog` installs packages from the [Bone catalog](https://github.com/vincentm65/bone-catalog): MCP servers, a file manager, Git diffs and reviews, sub-agents, skills, web search, themes and more.
- **Images.** Paste a screenshot with `ctrl+v` or attach a PNG, JPEG or WebP file with `/attach`, and send it to a vision model.
- **Long sessions.** Keep typing while it works: messages queue or steer the running turn. Shell commands can run in the background, every chat is saved and listed in a sidebar, and old context is compacted when the window fills.
- **One API.** The core speaks JSON-RPC 2.0 over stdio, Unix sockets or Windows named pipes. The TUI is one client; your scripts and editor can be others.

Tool calls run without asking by default. A Lua hook can ask first, refuse a
call or change it (see [docs/lua.md](docs/lua.md#asking-the-user)).

## Desktop app

![The bone desktop app: a sidebar of chats beside the same session, with tool calls and a diff](docs/images/desktop.png)

```sh
bone --gui        # open the desktop app
bone desktop      # add it to the Start menu, Applications or your app menu
```

The desktop app is the same agent in a native window: the same sessions,
commands, plugins and Lua config. Open it next to the TUI and they stay in
sync. `bone desktop --remove` takes the shortcut away again, and
`bone update` updates both.

## Make it yours

![The same session in a customized TUI with a file explorer, an activity timeline, a header and a different colorscheme](docs/images/custom.png)

This is the same binary with about 550 lines of Lua in `~/.bone`: a file
explorer, an activity timeline, a header, a bordered prompt and a
colorscheme. The TUI reloads its config when you save a file. A few lines
go a long way:

```lua
-- ~/.bone/tui.lua
bone.keymap.set("f2", "/sessions")
bone.hl.set("ToolPath", { fg = "#7dcfff", bold = true })
bone.cmd.create("hello", function(c) bone.notify("hi " .. c.args) end, { desc = "say hi" })
```

```lua
-- ~/.bone/core.lua: give the model a new tool
bone.tool.register {
  name = "git_log",
  description = "Show recent commits.",
  parameters = { type = "object", properties = { n = { type = "integer" } } },
  run = function(args, ctx)
    return bone.system("git log --oneline -n " .. (args.n or 10), { cwd = ctx.cwd }).stdout
  end,
}
```

bone writes its own guides to `~/.bone/docs/` and points the agent at them,
so you can also just ask it: "add a clock to the statusline".
[docs/lua.md](docs/lua.md) is the reference, and
[docs/customizing.md](docs/customizing.md) has a guide for each part.

## Plugins

![The /catalog screen listing installable plugins with their versions and descriptions](docs/images/catalog.png)

`/catalog` browses, installs, updates and removes packages, checking each
file against the catalog's hashes. Nothing is installed by default. Some of
what is there:

| Plugin | What it adds |
|---|---|
| MCP | add MCP servers, sign in with OAuth, browse their tools and turn them on and off |
| Files | a file manager, viewer, text editor and CSV table in a docked sidebar |
| Git Diff, Review | read-only diffs beside the chat, and Git reviews of your changes |
| Claude Code, Codex, Grok | use a Claude, ChatGPT or Grok subscription as the model provider |
| Fork | branch a session from any earlier message |
| Goal | keep a chat working on an objective, turn after turn, until it is done |
| Context | see what fills the context window |

A plugin is a folder in `~/.bone/plugins/` with any of `core.lua`, `ui.lua`,
`tui.lua`, `gui.lua` and its own modules, so writing one is the same work as
writing your config.

## Install

With npm, on Linux and macOS (x64 and arm64) and Windows (x64). No toolchain
is needed:

```sh
npm install -g bone-agent
bone                          # the TUI, in your project's folder
bone --gui                    # the desktop app
bone update                   # update to the newest version
```

`bone desktop` makes a Start menu and desktop shortcut on Windows,
`~/Applications/Bone.app` on macOS (Spotlight, Launchpad, the Dock) and an
app menu entry on Linux, all opening the desktop app from this install; it
starts in your home folder. `bone update` (or the prompt bone shows when a
new version is out) updates the shortcut's app too. Run `bone desktop`
again after changing `PATH` or Node version.

Linux reads the clipboard through `wl-paste` (Wayland, package wl-clipboard)
or `xclip` (X11); `/health` says when one is missing.

<details>
<summary>Build from source</summary>

You need Rust 1.88+ and a C toolchain (LuaJIT is built from source). The Lua
runtime is compiled into the binary; nothing else needs to be installed.

```sh
scripts/install.sh              # builds release, installs ~/.local/bin/bone3
scripts/install.sh --name bone  # install as `bone` instead
```

Or `cargo build --release` and run `target/release/bone`. Add
`-p bone --features gui` for a binary that also has the desktop app.

**Windows.** Build with `cargo build --release` in a Rust/C toolchain
terminal and run `target/release/bone.exe`. Shell tools use `cmd.exe`; live
shell jobs use ConPTY (Windows 10 version 1809 or newer). The default config
directory is `%USERPROFILE%\.bone`.

**Android/Termux.** Use Clang with its compiler runtime installed. The build
automatically discovers and links Clang's builtins archive for LuaJIT's
instruction-cache flush; no extra `RUSTFLAGS` are needed. For cross builds,
configure the Android C compiler through `CC_<target>` (for example,
`CC_aarch64_linux_android`) and the Rust linker as usual. Select Android API
23 or newer for the shell tool's `openpty` support.

</details>

## Quick start

```sh
cd your/project
bone                     # first run opens the guided provider setup
# Or configure a provider for one run:
BONE_BASE_URL=http://localhost:8080/v1 BONE_MODEL=qwen bone
```

On the first run, choose a local server or hosted provider in the setup
screen. API keys entered there are stored in `~/.bone/secrets.json`; settings
are stored in `~/.bone/settings.json`, and your Lua configuration is left
untouched. You can reopen the wizard with `/setup`, diagnose a connection
with `/health`, or change anything later with `/config`.

To set providers up in Lua instead, `bone --init` writes editable `core.lua`
and `tui.lua` starter files:

```lua
-- ~/.bone/core.lua
bone.config.providers.local_model = { base_url = "http://localhost:8080/v1", model = "qwen" }
bone.config.provider = "local_model"
```

Type a message and press enter. A good first test is `explain this project`
or `run the tests and fix any failures`. Type `/` for commands (`/help` lists
them), `ctrl+o` opens an earlier session, `ctrl+t` changes how much of each
tool call is shown, `ctrl+c` cancels a turn (twice on an empty prompt quits),
and `bone -r` picks up where you left off.

## One core, shared

Every `bone` and `bone --gui` without `--connect` shares one core: the first
launch starts it as a process of its own, later launches attach to it, and it
exits when its last client disconnects. Open the TUI and the desktop app side
by side and they stay in sync; close them in any order and nothing is left
running. A `bone --gui` while the desktop app is already running opens the
window there instead of starting over.

To run the core by hand, for scripts, editors, or a server that outlives its
clients:

```sh
bone --headless                 # serve the API on stdin/stdout (for scripts and editors)
bone --headless --listen        # serve on $XDG_RUNTIME_DIR/bone3/bone.sock until signalled
bone --connect                  # a TUI on that server; run several, they stay in sync
```

## Documentation

- [docs/usage.md](docs/usage.md): keys, commands, options.
- [docs/lua.md](docs/lua.md): configuring and extending with Lua (core and TUI), colors, plugins.
- [docs/customizing.md](docs/customizing.md): the customization guides (TUI, panels, popups, chat, core, plugins), written to `~/.bone/docs/` at startup (the agent's system prompt points there) and available to Lua as `bone.docs`.
- [docs/protocol.md](docs/protocol.md): the JSON-RPC API for writing clients.
- [docs/architecture.md](docs/architecture.md): how the pieces fit together.
- [Bone catalog](https://github.com/vincentm65/bone-catalog): the source of installable plugins. Use `/catalog` to install and update verified packages.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

- Protocol golden files live in `crates/bone-proto/tests/golden/`; after an intended wire change run `UPDATE_GOLDEN=1 cargo test -p bone-proto --test golden`.
- End-to-end TUI tests exercise the standard runtime UI against a real core and a fake model: `cargo test -p bone --test tui`.
- Image integration tests: `cargo test -p bone --test images`. On an isolated desktop, run native clipboard coverage with `cargo test -p bone --test images -- --ignored`; set `BONE_TEST_SCREENSHOT` to a captured PNG to use its pixels, or `BONE_TEST_BINARY` to the release executable to test that build. These ignored tests replace and clear the clipboard on the display they run on.
- Windows shell checks: `cargo test -p bone-core native_windows_shell`. Run `cargo test -p bone-core native_windows_shell_pty -- --ignored` on native Windows to validate ConPTY; Wine cannot validate its child console output.
- `scripts/dist.sh` builds `dist/bone-<version>-<target>.tar.gz`.

## License

MIT
