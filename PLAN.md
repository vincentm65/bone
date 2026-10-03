# bone3 - High-Level Plan

Ground-up rebuild of bone. Rust + Lua.

## Goals
1. **One way to call and connect to the core** - a single, stable API/protocol.
2. **Minimal, fullscreen TUI** - modeless and simple to use; everything is configurable and scriptable from Lua.

## Architecture

```
bone3/
  crates/
    bone-core/     agent loop, providers, tools, sessions, config (no UI)
    bone-proto/    the single API: typed messages (requests, events), serde
    bone-server/   hosts core, speaks bone-proto over a transport
    bone-client/   client lib: connect (in-process or socket), send/receive
    bone-tui/      fullscreen TUI (ratatui/crossterm) + embedded Lua (mlua, LuaJIT)
    bone-lua/      Lua API bindings
  runtime/         default Lua: keymaps, options, default UI, themes
  docs/
```

## Part 1: Core + single connection point
- Core is UI-agnostic: takes `Request`s, emits a stream of `Event`s.
- `bone-proto`: one versioned message schema, the only contract.
- One `Client` interface with interchangeable transports: in-process channel, Unix socket, stdio. TUI, CLI, scripts and editors all use it.
- Headless mode: `bone --headless` / `--listen <sock>` (like nvim).
- Core owns sessions, providers, tools, hooks, subagents.

## Part 2: TUI
- One view: the session above, the prompt below, a statusline. No modes; `/` commands with suggestions; popups (from Lua, e.g. approvals) and the session picker.
- Lua API (`bone.*`): options, keys, highlights/themes, event subscriptions, core requests, slash commands, statusline/divider and tool views.
- Config: `~/.bone/tui.lua`, modules and plugins; defaults ship in `runtime/` as plain Lua so users can replace any of it.
- Diff-based async rendering that never blocks on the core.

## Milestones
All seven are done (see README.md and docs/). What is left is in Open Questions below.

1. Skeleton: cargo workspace, draft `bone-proto`, echo server + client over in-process transport.
2. Core port: agent loop, one provider, core tools, sessions behind the proto API.
3. Transports: socket + stdio, headless mode, version handshake.
4. TUI base: fullscreen render loop, buffers/windows, modes, streaming chat.
5. Lua layer: embed mlua (LuaJIT) in both core and TUI, `bone.*` API per side, load init.lua, move defaults into `runtime/`.
6. Customization: themes, statusline, layouts, commands, autocmds, plugins.
7. Polish: docs, tests (proto golden tests, headless TUI tests), packaging.

## Decisions
- **Wire format:** JSON-RPC 2.0, newline-delimited JSON. serde types in `bone-proto` are the source of truth.
- **Lua scope:** Lua runs in both the core and the TUI. Core-side Lua can add tools, providers and hooks; TUI-side Lua controls the UI. Core-side scripts run with full trust (see below).
- **Existing code:** fully fresh design. Old bone is only a UX reference, not a code source.
- **Lua flavor:** LuaJIT (5.1 dialect) via `mlua`. Avoid LuaJIT-only features so 5.4 stays a fallback.
- **Lua runtimes:** separate Lua states for core and TUI, with a small shared pure-Lua utility layer. The sides talk only through the protocol.
- **Lua trust:** full trust, like Neovim. No Lua sandbox. Tool calls run without asking by default; asking first is a plugin (`examples/plugins/approve`).
- **Config files:** everything lives in `~/.bone/` (override with `$BONE_CONFIG_DIR`): `core.lua` (providers, tools, hooks, permissions, prompts), `tui.lua` (options, keymaps, theme, layout, commands, renderers), `lua/` for shared modules, `runtime/` for overriding built-in runtime files, and `sessions/`. Plugins can ship `core/` and `tui/` parts. `BONE_*` env vars override `core.lua` for one-off runs.
- **Tool formatting:** the protocol carries structured tool calls/results plus an optional `display` hint (`kind`, `data`). TUI Lua registers renderers by tool name or display kind. Core Lua hooks transform what the model sees.

## Resolved
- **No Vim modes (decided after milestone 7):** the TUI was first built Neovim-style (modes, `:` commands, splits, `<C-w>` keys). It is now modeless: one chat plus the prompt, `/` commands with suggestions, popups and the session picker, and keys written as `"ctrl+s"` with per-popup contexts instead of modes. The architecture (core/protocol/clients, two Lua states, runtime defaults, plugins) is unchanged.
- **Tool display:** no display hint in the protocol for now. The TUI renders built-in tools from their call and result (condensed headers, previews, diffs) and Lua replaces any tool's view by name with `bone.ui.tool_views[name]`, returning a title and rows of `{text, highlight group}`.
- **Plugin layout:** `~/.bone/plugins/<name>/{core.lua,tui.lua,lua/,colors/}`, loaded in name order after the runtime and before the user's files; `_`/`.` prefixes disable. Installing is copying or cloning; no plugin manager yet.

## Open Questions
- Hook set for core Lua beyond `tool_call` / `tool_result` (message, session events).
- A plugin manager (install/update from git) if copying folders gets tedious.
- **Hooks and approval:** the core has no approval logic. Generic hooks run at every step of a turn (`turn_start`, `request`, `message`, `tool_call`, `tool_result`, `turn_end`, plus custom points via `bone.run_hooks`); any hook or Lua tool can wait on the user with `bone.ask` (`ask/requested` / `ask/respond` / `ask/resolved`), and TUI Lua shows questions with `bone.ui.popup`. Approval is an opt-in plugin (`examples/plugins/approve`); by default every tool call runs. Nothing ships as a built-in plugin.
- **Blank slate:** the TUI has no built-in look. Rust draws the chat as plain text and the bare prompt; the statusline and divider rows exist only when Lua defines them, `bone.ui.prompt` sets the prompt prefix/placeholder, and Lua popups are drawn entirely by Lua (Rust places and clears them). The previous default look lives in `examples/plugins/style`, the start of a styles package that may later ship as the default.
- **Neovim-level customization (after milestone 7):** Lua waits without blocking (`bone.system`/`sleep`/`http`/`http_stream` in core jobs, callbacks in the TUI); windows (`bone.ui.win`: focus, anchor, z, update) and a Lua `bone.ui.layout`; the session picker (`bone.ui.select`) and `/` suggestions are Lua; `/help {topic}` over the embedded docs and `/health` (`health/check` + `bone.health`); model providers can be written in Lua (`bone.provider.register`, `type = ...`).

## Extensibility roadmap (Lua API v1)

The customization work is additive and is delivered in small phases. The
core and protocol remain the authority for sessions and turns; TUI Lua owns
presentation, input and client-side workflows. Each phase keeps the v1
compatibility wrappers documented in `docs/lua.md` and reports new behavior
through `bone.has_capability`.

## Current execution status

The work is being tracked as phases 0–8 (the roadmap entries below begin at
phase 1). Phase 0, the baseline, is committed as `b783dbd`. Phase 1, the
extension contract plus dynamic contexts and input, is committed as
`4514d1f` and `a55c154`. Phase 2, events, commands and options, is
committed as `11dd866`. Phase 3, stateful panels (`tui.panels`), is
committed as `e13978c`.

Phase 4, prompt and chat data (`tui.prompt_edit`, `tui.chat_data`), is
committed as `0462c61`.

Phase 5, cancellable streaming jobs (`jobs.streaming`), is committed as
`9f439dd`.

Phase 6, **plugin state and lifecycle**, is complete in the current
worktree. It adds:

- `bone.state.load/save` on both sides (JSON under `state/<side>/`, written
  atomically) and `bone.plugin.current()` while plugin files load
  (`plugins.state`);
- TUI ownership of keymaps, commands, event handlers, panels, windows,
  jobs, dynamic options, raw interceptors and contexts created while a
  plugin's file or callbacks run, with identity checks so user overrides
  survive (`plugins.lifecycle`);
- `bone.plugin.on_shutdown`, `load`/`unload`/`reload` (also `/plugin`),
  module cache invalidation on unload, auto-saved `bone.plugin.state`,
  `plugin/loaded`/`plugin/unloaded` events, and shutdown hooks plus state
  saving when the TUI quits;
- explicit project configuration: `<project>/.bone/tui.lua` found from the
  working directory upward, run only after `/project trust` (remembered in
  TUI state), unloadable with `/project untrust` (`tui.project`); and
- tests for the state store, plugin loading markers, ownership cleanup,
  reload with changed modules, persisted state and project trust.

Core-side plugin unload/reload is not included: the core Lua state lives
for the whole server and hooks/tools are not owner-tracked. Folder loading
is unchanged. The remaining phase covers representative plugins plus final
validation. The out-of-scope boundaries below continue to apply.

1. **Extension contract and compatibility** — publish `bone.api_version`,
   capability reporting, ownership rules, and the baseline contracts for
   contexts, events, commands, options, panels, jobs and plugin state.
2. **Dynamic contexts and input** — add named contexts, fallback/priority,
   key sequences, consumption/pass-through, raw interception and real
   `timeoutlen` handling while retaining modeless `main`/`popup` behavior.
3. **Events, commands and options** — add local UI events, command aliases and
   completion/structured arguments, and dynamic typed options with callbacks.
4. **Stateful panels** — add persistent plugin panels with IDs, rendering and
   key callbacks, focus, lifecycle, docking, sizing and scrolling. Keep
   `bone.ui.win` as the overlay compatibility API.
5. **Prompt and chat data** — expose cursor/selection/range prompt edits and
   structured read-only session/turn/message/tool data without becoming a
   general file editor.
6. **Cancellable streaming jobs** — add process handles, streamed output,
   cancellation, timeouts, completion/error callbacks and status reporting.
7. **Plugin state and lifecycle** — add init/shutdown/cleanup, persistent
   per-plugin state and explicit project configuration while retaining folder
   loading.
8. **Representative plugins and validation** — build Lua-only task, review,
   provider-switching and streaming-test workflows; then run focused tests,
   formatting, clippy, the release build and the full workspace suite.

Features are intentionally capability-gated until their phase is complete.
Vim modes, full editor buffers, LSP/syntax infrastructure, arbitrary cell
drawing and a plugin manager remain out of scope unless a representative
workflow demonstrates that one is necessary.
