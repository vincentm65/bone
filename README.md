# bone

A terminal coding assistant: a small Rust core that talks to any
OpenAI-compatible model, a fullscreen TUI, and Lua for everything you'd
want to change.

- **One API.** The core speaks JSON-RPC 2.0 over stdio or a Unix socket. The TUI is just a client; scripts and editors can be too.
- **A simple TUI.** Type and press enter. `/` commands with suggestions, streaming output, and a session picker. Tool calls run without asking; Lua hooks can add confirmation prompts (see [docs/lua.md](docs/lua.md#asking-the-user)).
- **Lua all the way down.** `~/.bone/core.lua` sets up providers (or writes new ones with the Lua provider API), tools and hooks; `~/.bone/tui.lua` keys, commands, colors, the statusline and how tool calls look. The screen starts with the standard UI in `runtime/lua/bone/ui/`, replaceable piece by piece.

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

On Android/Termux, use Clang with its compiler runtime installed. The build
automatically discovers and links Clang's builtins archive for LuaJIT's
instruction-cache flush; no extra `RUSTFLAGS` are needed. For cross builds,
configure the Android C compiler through `CC_<target>` (for example,
`CC_aarch64_linux_android`) and the Rust linker as usual. Select Android API
23 or newer for the shell tool's `openpty` support.

## Quick start

```sh
cd your/project
bone3                    # first run opens the guided provider setup
# Or configure a provider for one run:
BONE_BASE_URL=http://localhost:8080/v1 BONE_MODEL=qwen bone3
```

On the first run, choose a local server or hosted provider in the setup screen. API keys entered there are stored in `~/.bone/secrets.json`; settings are stored in `~/.bone/settings.json`, and your Lua configuration is left untouched. You can reopen the wizard with `/setup`, diagnose a connection with `/health`, or customize everything later with `/config`.
```

A provider is any OpenAI-compatible `/chat/completions` endpoint: a local llama.cpp / vLLM / tabbyAPI server, DeepSeek, OpenRouter, OpenAI and so on. For advanced users, `bone3 --init` still creates editable `core.lua` and `tui.lua` starter files.

```lua
-- ~/.bone/core.lua
bone.config.providers.local_model = { base_url = "http://localhost:8080/v1", model = "qwen" }
bone.config.provider = "local_model"
```

For a one-off run without a config: `BONE_BASE_URL=http://localhost:8080/v1 BONE_MODEL=qwen bone3`.

Type a message and press enter. A good first test is `explain this project` or `run the tests and fix any failures`. Tool calls run without asking unless you add an approval hook. Type `/` for commands (`/help` lists them), `ctrl+o` opens an earlier session, `ctrl+c` cancels a turn (twice on an empty prompt quits), and `bone3 -r` picks up where you left off.

## Running the core separately

```sh
bone3 --headless                 # serve the API on stdin/stdout (for scripts and editors)
bone3 --headless --listen        # serve on $XDG_RUNTIME_DIR/bone3/bone.sock
bone3 --connect                  # a TUI on that server; run several, they stay in sync
```

## Documentation

- [docs/usage.md](docs/usage.md): keys, commands, options.
- [docs/lua.md](docs/lua.md): configuring and extending with Lua (core and TUI), colors, plugins.
- [docs/customizing.md](docs/customizing.md): the customization guides (TUI, panels, popups, chat, core, plugins), written to `~/.bone/docs/` at startup (the agent's system prompt points there) and available to Lua as `bone.docs`.
- [docs/protocol.md](docs/protocol.md): the JSON-RPC API for writing clients.
- [docs/architecture.md](docs/architecture.md): how the pieces fit together.
- [Bone catalog](https://github.com/vincentm65/bone-catalog): the source of installable Bone 3 plugins. Use `/catalog` to install and update verified packages.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

- Protocol golden files live in `crates/bone-proto/tests/golden/`; after an intended wire change run `UPDATE_GOLDEN=1 cargo test -p bone-proto --test golden`.
- End-to-end TUI tests exercise the standard runtime UI against a real core and a fake model: `cargo test -p bone --test tui`.
- `scripts/dist.sh` builds `dist/bone-<version>-<target>.tar.gz`.
