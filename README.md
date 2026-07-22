# bone

**A terminal coding agent that stays out of your way.**

Bone can inspect a codebase, edit files, run commands, and keep working in the background while you stay in control. The default interface is a fast native TUI, with headless, daemon, and web modes available when you need them.

[![npm](https://img.shields.io/npm/v/bone-agent?label=npm)](https://www.npmjs.com/package/bone-agent)
[![release](https://github.com/vincentm65/bone/actions/workflows/npm-release.yml/badge.svg)](https://github.com/vincentm65/bone/actions/workflows/npm-release.yml)

## Install

The npm package ships a native binary for Linux, macOS, and Windows:

```sh
npm install -g bone-agent
```

Then open a project and run:

```sh
cd your-project
bone
```

The first launch walks you through provider setup. You can run the wizard again at any time with `bone setup`.

### Build from source

Bone uses stable Rust:

```sh
git clone https://github.com/vincentm65/bone.git
cd bone
cargo build --release
./target/release/bone
```

Run `bone install` to add the built binary to your `PATH`.

## What it does

- Reads, searches, creates, and edits files with reviewable diffs
- Runs shell commands and manages long-running background processes
- Streams responses, reasoning, tool activity, and command output as they happen
- Keeps conversations and usage history locally in SQLite
- Supports background sub-agents for independent work
- Works with Anthropic, OpenAI, Codex, Grok Build, and OpenAI-compatible APIs, including local models
- Uses a safe approval mode by default, with an opt-in danger mode for trusted work
- Can be extended with Lua tools, commands, keymaps, themes, hooks, and UI panes

## Ways to run Bone

```sh
# Interactive terminal UI
bone

# Pick a provider or model
bone --provider anthropic --model claude-sonnet-4-6

# Run one headless turn
bone run --prompt "find the cause of the failing tests"

# Start the multi-client daemon
bone serve

# Attach the TUI to a daemon
bone --connect 127.0.0.1:7878

# Open the web interface
bone web
```

`bone serve` listens on `127.0.0.1:7878` by default. It uses unencrypted, unauthenticated TCP, so do not expose it to an untrusted network.

## Web interface

`bone web` starts the local bridge and opens the browser UI at `http://localhost:4577`. It includes conversation history, provider and model controls, approvals, attachments, usage stats, and a file/diff canvas.

Node.js is required for web mode. See [`webui/README.md`](webui/README.md) for bridge details and environment variables.

## Configuration

Bone keeps its local state in `~/.bone-rust` by default. Set `BONE_DIR` to use another location.

```text
~/.bone-rust/
├── config.yaml             # general, UI, theme, keymap, and enablement values
├── providers.yaml          # providers, models, endpoints, and credentials
├── subagents.yaml          # named subagent definitions and prompts
├── extensions.yaml         # namespaced Lua extension values
├── command-policy.yaml     # shell approval rules
├── lua/                    # custom tools, commands, and libraries
└── data/                   # conversations and runtime state
```

Core owns these documents and exposes one revisioned configuration schema and resolved snapshot to every client. Use `/config` or the web settings panel for supported generic mutations; providers use dedicated client actions, while themes and keymaps also have dedicated Lua APIs. Built-in schemas live in Rust; extensions declare schemas with `bone.settings.define(namespace, schema)`, while YAML stores only user-selected values. Provider secrets may be plaintext or exact `${ENV_VAR}` references, which resolve from the environment at runtime.

Most changes apply immediately or on the next model turn. Extension settings may request an extension reload. Direct edits are loaded at startup; `command-policy.yaml` is file-edited, daemon-owned, and always requires a restart. Provider entries support the native Anthropic, Codex, and Grok Build handlers as well as OpenAI-compatible endpoints.

Optional tools and commands can be managed with:

```sh
bone catalog
```

## Approval modes

Bone starts in **Safe** mode. Read-only work can proceed automatically, while file writes and non-read-only shell commands ask for approval. **Danger** mode skips those prompts and is intended only for environments you trust.

`command-policy.yaml` controls which shell commands Bone classifies as read-only.

## Architecture

The daemon is the source of truth for sessions, tools, approvals, extensions, jobs, and persistence. The terminal and web interfaces are clients of the same runtime protocol.

```text
                    ┌──────────────┐
TUI ───────────────▶│              │
Headless runner ───▶│  Bone core   │──▶ model providers
Web bridge ────────▶│  + runtime   │──▶ tools / Lua / SQLite
Remote client ─────▶│              │
                    └──────────────┘
```

The Rust workspace is split into:

- `core` — agent loop, providers, tools, configuration, Lua, runtime, and persistence
- `protocol` — frontend/runtime commands and events
- `tui` — the `bone` binary and terminal client
- `webui` — zero-dependency Node bridge and browser client

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo build --release
```

The web client does not need an npm build step. Run it against a local build with:

```sh
BONE_BIN=target/debug/bone node webui/bridge.mjs
```
