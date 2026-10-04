# bone

A terminal coding assistant: a small Rust core that talks to any
OpenAI-compatible model, a fullscreen TUI, and Lua for everything you'd
want to change.

- **One API.** The core speaks JSON-RPC 2.0 over stdio or a Unix socket. The TUI is just a client; scripts and editors can be too.
- **A simple TUI.** Type and press enter. `/` commands with suggestions, streaming output, and a session picker. Tool calls run without asking; `examples/plugins/approve` adds a y/n prompt before anything changes your files.
- **Lua all the way down.** `~/.bone/core.lua` sets up providers (or writes new ones in Lua: see `examples/plugins/anthropic`), tools and hooks; `~/.bone/tui.lua` keys, commands, colors, the statusline and how tool calls look. The screen starts with the standard UI in `runtime/lua/bone/ui/`, replaceable piece by piece; `examples/plugins/style` is another complete look to install or copy from.

```text
› fix the typo in main.rs

  I'll look at the file first.
    read_file main.rs (4 lines)
    edit_file main.rs (+1 −1)
    │ - println!("helo");
    ╰ + println!("hello");
  Fixed: helo → hello.
────────────────────────────────────────────────────────────────────
› Message bone…
 fix the typo in main.rs                 1.1k in · 20 out  │  ~/project
```

## Install

Needs Rust 1.88+ and a C toolchain (LuaJIT is built from source).

```sh
scripts/install.sh            # builds release, installs ~/.local/bin/bone3
scripts/install.sh --name bone  # install as `bone` instead
```

Or just `cargo build --release` and run `target/release/bone`. The Lua runtime is compiled into the binary; nothing else needs to be installed.

## Quick start

```sh
bone3 --init          # writes ~/.bone/core.lua and ~/.bone/tui.lua
$EDITOR ~/.bone/core.lua   # pick a provider
cd your/project && bone3
```

A provider is any OpenAI-compatible `/chat/completions` endpoint: a local llama.cpp / vLLM / tabbyAPI server, DeepSeek, OpenRouter, OpenAI and so on.

```lua
-- ~/.bone/core.lua
bone.config.providers.local_model = { base_url = "http://localhost:8080/v1", model = "qwen" }
bone.config.provider = "local_model"
```

For a one-off run without a config: `BONE_BASE_URL=http://localhost:8080/v1 BONE_MODEL=qwen bone3`.

Type a message and press enter. Tools that change things (`write_file`, `edit_file`, `shell`) ask first: `y` allows, `n` denies. Type `/` for commands (`/help` lists them), `ctrl+o` opens an earlier session, `ctrl+c` cancels a turn (twice on an empty prompt quits), and `bone3 -r` picks up where you left off.

## Running the core separately

```sh
bone3 --headless                 # serve the API on stdin/stdout (for scripts and editors)
bone3 --headless --listen        # serve on $XDG_RUNTIME_DIR/bone3/bone.sock
bone3 --connect                  # a TUI on that server; run several, they stay in sync
```

## Documentation

- [docs/usage.md](docs/usage.md): keys, commands, options.
- [docs/lua.md](docs/lua.md): configuring and extending with Lua (core and TUI), colors, plugins.
- [docs/protocol.md](docs/protocol.md): the JSON-RPC API for writing clients.
- [docs/architecture.md](docs/architecture.md): how the pieces fit together.
- [catalog/README.md](catalog/README.md): the native Bone 3 catalog and installer.
- [examples/plugins/git](examples/plugins/git): a small plugin with a tool, a tool view and a command.
- More examples in [examples/plugins](examples/plugins): `switch` (change provider from the TUI), `tasks` (a task panel), `review` (changed files and review prompts), `stats` (tokens, sessions and tool failure rates from the session index), `testrun` (tests streamed into a panel), `compact` (summarize long sessions), `output-cap` (limit tool result sizes), `retry` (backoff and provider fallback), `mcp` (MCP servers from JSON files), `skills` and `templates` (folders of them, with TUI commands), `ask-model` (side questions), `approve`, `style` and `anthropic`. None is installed by default.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

- Protocol golden files live in `crates/bone-proto/tests/golden/`; after an intended wire change run `UPDATE_GOLDEN=1 cargo test -p bone-proto --test golden`.
- End-to-end TUI screens are compared with `crates/bone/tests/snapshots/`; after an intended UI change run `UPDATE_SNAPSHOTS=1 cargo test -p bone --test tui`.
- `scripts/dist.sh` builds `dist/bone-<version>-<target>.tar.gz`.
