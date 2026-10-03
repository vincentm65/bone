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

Phase 6, plugin state and lifecycle (`plugins.state`,
`plugins.lifecycle`, `tui.project`), is committed as `37d0c59`.

Phase 7, **representative plugins and validation**, is complete in the
current worktree. Four Lua-only plugins in `examples/plugins/` exercise the
new APIs end to end, each with tests:

- `tasks`: a persistent task panel (panels with keys, `bone.plugin.state`,
  `bone.prompt`);
- `review`: the files the session changed, git diffs and review prompts
  (`bone.chat` data, panels, `bone.prompt` selection, `bone.job`);
- `switch`: provider switching from the TUI through a Lua router provider
  (OpenAI-compatible client in Lua, delegation to Lua provider types); and
- `testrun`: tests streamed into a following panel with cancel and "send
  failures to the model" (`bone.job` lines, panels, dynamic options).

Building them exposed two gaps, filled additively: `bone.state` gained a
`{ shared = true }` scope for a plugin's core and TUI halves, and the core
gained `bone.on_ready` (`core.ready`) so a plugin can read the final
`bone.config` after the user's `core.lua`. It also caught a `testrun` bug
(the echoed command counted as a failure). Validation: `cargo fmt --all --
--check`, `cargo clippy --workspace --all-targets` (no warnings),
`cargo test --workspace`, `cargo build --release` and `git diff --check`.

All phases of the extensibility roadmap are done. The out-of-scope
boundaries below continue to apply; core-side plugin unload/reload remains
a possible follow-up.

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

## Agent-side extensibility roadmap (Lua API v1, part 2)

Goal: close the gaps with agents such as pi on the core side, as mechanisms
only. Everything below is opt-in: the core gains the machinery (registries,
hook points, protocol methods), but a fresh install registers no skills,
templates, MCP servers or extra hooks, and its behaviour is unchanged until
`core.lua` or a plugin turns something on. Each mechanism reports a
capability, and the core keeps working when it is unused.

What exists today and shapes the order: the core reads its provider, tool
registry and hook set once at load (`Inner.provider`, `Inner.tools`,
`Scripting.hooks`), only the selected provider entry is ever built, and all
core Lua runs on one Lua thread through jobs. The existing `request` hook
already rewrites messages and tools per model call; what is missing is
persistent context changes, composable system prompts, error recovery and
stream observation.

### Phase 8: a swappable core runtime, and core plugin reload (gap 5)

Everything after this needs registries that can change while the core runs,
so this comes first.

- Move what core Lua produces into one immutable `Runtime` snapshot: the
  Lua thread (`Scripting`), the provider set, the tool registry, the hook
  set and the derived config. `Inner` holds `RwLock<Arc<Runtime>>`. Each turn
  takes the current `Arc<Runtime>` when it starts and keeps it until it
  ends, so a reload never changes a running turn.
- Reload rebuilds instead of patching: start a new Lua thread, load the
  runtime, then the plugins that are enabled, then `core.lua`, then run
  `on_ready`. Only if that succeeds is the new snapshot swapped in. The old
  one keeps serving its in-flight turns and pending `bone.ask` questions,
  and drops once they finish. A failed reload keeps the old runtime and
  reports the error.
- Unloading or loading a single plugin is a rebuild with that plugin left
  out of, or added to, the enabled set. Disabled names live in memory and,
  if asked, in `state/core/plugins.json`. Lua globals do not survive a
  reload, so plugins keep anything that must persist in `bone.state`.
  `bone.on_shutdown(fn)` runs on the outgoing runtime before the swap.
- Settings that cannot change live (`data_dir`) are ignored on reload, with
  a warning.
- The hook set becomes dynamic: `bone.hook` calls into Rust when it
  registers, instead of being read once by `extract`.
- Protocol: `core/reload`, `plugin/list` (core side), and
  `plugin/load|unload|reload` (each a rebuild), plus a `core/reloaded`
  event. The TUI's `/plugin` command lists and reloads both halves of a
  plugin. Capability `core.reload`.
- Tests: reload while a turn runs (the turn finishes on the old runtime and
  the next one uses the new); a failed reload keeps the old runtime;
  unloading a plugin drops its tools and hooks; state survives through
  `bone.state`.

### Phase 9: more core hook points (gap 1)

New points, all opt-in. A point with no hooks costs nothing (the dynamic
hook set is checked first).

- `system` `{ session_id, cwd, prompt }` composes the system prompt. Every
  plugin can append to or replace it. `bone.config.system_prompt` remains
  the base.
- `context` `{ session_id, messages }` runs before every model call, after
  `system` and before `request`. It is the documented place to rewrite
  history for that call. `request` keeps its meaning.
- `request_error` `{ session_id, error, attempt, provider }` runs when a
  model call fails. It may return `{ retry = ms }` or
  `{ retry = ms, provider = name }` (which needs phase 10's provider set),
  or nothing to fail the turn. The core caps attempts.
- `stream` `{ session_id, kind, text }` lets hooks watch deltas. Its results
  are ignored, and it runs as a batched, fire-and-forget job so it never
  slows the stream. It is only active while registered.
- `session_start` `{ session_id, cwd, new }` and `session_end`
  `{ session_id }` (closed or evicted) are lifecycle hooks; their results
  are ignored.
- `message` also gets `usage`.
- Hook options: `bone.hook(name, fn, { priority = n })` controls the order
  hooks run in (the default 0 keeps registration order).
- Session writes for hooks and tools (core-side Lua, only between model
  calls):
  - `bone.session.messages(id)`;
  - `bone.session.append(id, message)` injects a message the model will
    see;
  - `bone.session.compact(id, messages)` stores a compaction checkpoint
    record in the session file. Loading uses the newest checkpoint, the full
    history stays on disk, and clients get a `session/compacted` event and
    reload the transcript.

  Compaction itself is a plugin (built on phase 10's model calls), not a
  default.
- Capabilities: `core.hooks.system`, `core.hooks.context`,
  `core.hooks.errors`, `core.hooks.stream`, `core.hooks.session`,
  `core.session_write`.
- Tests: for each point, the order across priorities, a retry that switches
  provider, compaction surviving a restart, and the `stream` hook not
  blocking deltas.

### Phase 10: Lua can call a model (gap 2)

- The runtime builds every `bone.config.providers` entry into a provider (a
  model registry), not only the selected one. The selected entry stays the
  agent's default.
- Core Lua: `bone.model.complete(req)` is a job wait. `req` takes:
  - `provider` (default: the current provider);
  - `messages`, or `prompt` with an optional `system`;
  - `tools` (specs only; nothing is run);
  - `options` (overrides such as `model`, `max_tokens`,
    `reasoning_effort`);
  - `on_delta(d)`.

  It returns `{ content, reasoning, tool_calls, usage }`. Called inside a
  turn, it is cancelled with that turn.
- `bone.model.list()` returns `{ name, model, type, current }`. Calls nest
  at most 4 deep, so a Lua provider cannot recurse forever.
- Protocol, so TUI Lua and other clients can use it:
  - `model/list`;
  - `model/complete { provider?, messages, tools?, options?, stream? }`;
    with `stream = true` the core sends `model/delta { request_id, kind,
    text }` notifications to that connection only;
  - `model/cancel`.

  The TUI exposes these as `bone.model.complete(req, on_delta, on_done)`.
- Capabilities `core.model` and `tui.model`.
- Tests: calls against a fake SSE server, through a Lua provider, with
  cancellation, the nesting limit, and the streaming protocol.

### Phase 11: MCP client (gap 3)

- A Rust `mcp` module in bone-core: a JSON-RPC client over a generic
  transport, so tests can run in memory. It covers stdio (spawned process)
  and, after that, Streamable HTTP; the `initialize` handshake; `tools/list`
  with `list_changed`; `tools/call`; and timeouts and cancellation that
  follow the turn.
- Opt-in only: no servers are configured by default.
  `bone.mcp.add(name, { command, args, env, cwd } | { url, headers },
  { tools = { allow, deny }, lazy = true, timeout })` registers one server.
  `bone.mcp.load(path)` imports the common `mcpServers` JSON format, and only
  when called. `bone.mcp.remove(name)`.
- Server lifecycle lives outside the Lua runtime (an `McpManager` in
  `Inner`). A reload compares the configured servers with the running ones,
  so servers that did not change keep running. A server that crashes
  restarts with backoff, and its status shows in `/health`.
- MCP tools join the dynamic registry as `<server>_<tool>`, with their
  annotations kept (a `readOnlyHint` server tool skips the approve plugin).
  They pass through the `tool_call`/`tool_result` hooks like any tool.
- `bone.mcp.call(server, tool, args)` is a job wait, for plugins.
  `bone.mcp.list()`. Resources and prompts come later; MCP prompts can feed
  phase 12's template registry.
- Protocol: `mcp/list` (servers, state, tools). Capability `core.mcp`.
- Tests:
  - an in-memory fake server for handshake, list, call, errors and
    `list_changed`;
  - one stdio test against a small fixture binary;
  - reload keeping unchanged servers alive;
  - cancellation during a call.

### Phase 12: skills and prompt templates (gap 4)

Registries in the core, empty by default.

Skills:

- `bone.skill.register { name, description, content | path }` (a file, or a
  folder with `SKILL.md`), with an optional `enabled(ctx)` predicate (for
  example, only in some projects). `bone.skill.load_dir(dir)` registers each
  `*/SKILL.md` it finds (front matter: `name`, `description`), and only when
  called. Also `bone.skill.unregister` and `bone.skill.list`.
- While at least one skill is enabled for a session, the core:
  1. adds a short "available skills" section (name and description) through
     the `system` hook point;
  2. registers a `skill` tool that returns a skill's full content and the
     paths of its bundled files.

  With no skills there is no section and no tool. Both can be switched off
  (`bone.config.skills = { prompt = false, tool = false }`) so a plugin can
  present skills its own way.
- Protocol: `skill/list`. Capability `core.skills`.

Prompt templates:

- `bone.template.register { name, description, args, body }`, where `body`
  is a string with `$1`, `$@` and `{{name}}` placeholders, or
  `function(args, ctx)` run as a job (so it can use `bone.system` and
  friends). `bone.template.load_dir(dir)` registers markdown files with
  front matter, when called. Also `unregister` and `list`.
- Protocol: `template/list`, `template/expand { name, args, session_id? }`
  returning the text, and a `template/changed` event. Expansion happens in
  the core, so every client gets the same text. Capability
  `core.templates`.
- The TUI gets `bone.skills.list()`, `bone.templates.list()` and
  `bone.templates.expand(name, args, cb)`. The TUI ships no default
  commands for them.

Tests: no skills means an unchanged prompt and tool list; an enabled skill
adds both; `enabled(ctx)`; `load_dir` parsing; placeholder substitution;
Lua bodies that wait; changes reaching clients.

### Phase 13: example plugins and validation

Lua-only examples that prove the mechanisms, none installed by default:

- `compact`: summarises old turns with `bone.model.complete` and
  `bone.session.compact`; a `/compact` command; optional automatic
  compaction from `request_error` when the context is too long.
- `retry`: backs off and retries, or falls back to another provider
  (`request_error`).
- `mcp`: loads an `mcpServers` file named in `core.lua`; its TUI half adds a
  server status panel.
- `skills`: `load_dir` on chosen folders; its TUI half adds `/skills` and
  `/skill name` (which asks the model to use that skill).
- `templates`: `load_dir`; its TUI half turns every template into a `/name`
  command with argument completion that expands into the prompt.
- `ask-model`: a TUI command that sends the selection to a side model with
  `bone.model.complete` and shows the answer in a pager.

Then the full validation run, as in phase 7.

Order and size: 8 is the foundation, a refactor of `Inner`, of how turns
pick up the runtime, and of the scripting loader. 9 and 10 can follow in
either order (10's provider fallback inside `request_error` needs both). 11
and 12 are independent of each other. 13 comes last. Rough size: 8 and 11
are the largest; 9, 10 and 12 are medium each.

Protocol: every addition is a new method or event. Existing methods keep
their shapes, and each addition gets golden tests and `docs/protocol.md`
entries. The protocol version stays the same, and clients check the
`core.*` capabilities, which the handshake will report.
