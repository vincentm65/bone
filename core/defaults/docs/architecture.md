# Core Architecture

Bone has one runtime authority: `core`. Frontends are clients of that runtime,
not alternate agent implementations.

```text
TUI / desktop client / headless runner / remote client
                         │ commands and events
                         ▼
                 core runtime + Driver
              ┌──────────┼───────────┐
              ▼          ▼           ▼
          providers     tools       Lua
                         │
                         ▼
                    SQLite state
```

## Workspace boundaries

- `core` owns the agent loop, providers, tools, approvals, configuration, Lua
  extensions (plain files and directory-based plugins under `lua/plugins/<name>/`),
  runtime sessions, jobs, and persistence.
- `protocol` owns the serializable commands, events, configuration snapshots,
  session snapshots, tool types, and view types that cross a frontend boundary.
- `tui` owns the native terminal client and its rendering/input code.
- `client` owns the shared lightweight JSONL/socket transport used by thin remote
  frontends; it does not contain daemon/session logic.
- `native` owns the `bone-desktop` eframe UI and local event reduction.

Keep dependencies flowing toward `core` and `protocol`. Core must not depend on
terminal rendering details.

## Driver and session ownership

`RuntimeSession` owns the truth for one conversation across turns:

- the model-facing transcript;
- tool handlers and session-scoped tool state;
- cumulative token accounting;
- the active SQLite conversation and message sequence; and
- the shared cancellation/steering state used by the runtime.

A `Driver` is built from that state for one turn. It owns the single provider/tool
loop, approval gate, extension manager, event sinks, and cancellation handling.
Its `DriverOutcome` is folded back into `RuntimeSession`, which persists the
result. Do not reimplement the turn loop in a frontend.

The daemon owns `RuntimeSession` and constructs `Driver`s. This is true both for
the in-process daemon used with the TUI and for standalone `bone serve`. A TUI
or desktop client sends commands and renders events; it does not own authoritative
transcript, approval, job, or configuration state.

## Turn execution

A submitted turn may make multiple normal provider requests while executing tools.
Before those requests, hooks run and may shape the request. Compaction is implemented
in catalog Lua rather than as dedicated Rust policy. Lua owns thresholds, history
selection, prompts, repair, checkpoint formatting, continuation wording, notices,
and replacement policy. It supplies explicit messages, tools, and an optional positive
`max_tokens` to `ctx.llm.complete`, which performs exactly one private provider request
with no agent/tool loop. Private text is not surfaced, and returned tool calls are
exposed to Lua without execution. Cancellation and usage are accounted by the
authoritative Driver turn or daemon command path.

Transcript mutation occurs only when a validated `conversation.replace` result is
applied. Automatic compaction applies it in the active Driver turn; manual `/compact`
returns the same generic action for the frontend to send to the daemon. Success replaces
model-facing history with a Lua-formatted checkpoint and retained recent turns. The
effective checkpoint is persisted to SQLite while complete display history remains
intact. Compaction-specific behavior is not a Rust runtime primitive or public event.

`bone run` slash-command expansion intentionally has no private completion access
because it has no durable conversation or command usage owner. Headless `before_turn`
hooks still run inside the authoritative Driver and may use private completion.

## Data flow

1. A client sends a `RuntimeCommand` through the local or newline-JSON runtime
   connection.
2. The daemon routes it to the conversation actor/session.
3. The session builds a `Driver` for a turn or applies a state mutation.
4. The driver streams provider, tool, approval, view, and status events.
5. The session applies the outcome and persists durable messages and usage.
6. Every attached client receives the same authoritative state/events needed to
   render its view.

Managed conversation actors retain a live attachment projection rather than a
copy of their boot values. A newly attached client therefore receives the
actor's current provider/model, extension frontend state, transcript/session
snapshot, and complete `ViewSnapshot`. After `StreamLagged`, a client sends
`Synchronize`; the actor includes the complete view inside the correlated
`StateSynchronized` response so a second lag cannot retain the completion while
dropping the pane/status/highlight repair.

Conversation actors may run independently, while clients attached to one
conversation observe its shared actor and event stream. Cancellation and
approvals are conversation-scoped.

## Durable versus ephemeral state

The core-owned SQLite tables are the durable source for conversations, messages, and
runtime records. The model-facing transcript may be a compacted/effective view; display history
remains complete. In-memory driver state, live view components, status text,
cancellation flags, and pending events are ephemeral and must not be treated as
persisted configuration.

Configuration has a separate authority: the daemon's `ConfigStore` loads and
validates the canonical YAML domains, produces one revisioned snapshot, and
broadcasts it to clients. See [Configuration](configuration.md).

Daemon-global storage operations use one `HostService`: usage statistics,
catalog reads/mutations, and setup are correlated `HostRequest`/`HostResponse`
operations. The service is shared by conversation actors, while each actor
keeps an isolated Lua VM. A catalog change reloads every cached actor without
letting `ConfigStore` retain those VMs. Frontends keep the fullscreen workflow
and rendering, but never substitute their own database or config directory.

Building an actor's Lua VM is an ordered boot: seed library modules and the
`lua/helpers/` directory → run `init.lua` (config root, then `lua/init.lua`) →
seed default tools/commands → run tool files → run command files → run enabled
plugins (`lua/plugins/*/init.lua`, one level, sorted) → collect the registered
tools and commands. A plugin is skipped when disabled via the
canonical `plugins.<name>` setting; disabling never deletes files, and because
reload rebuilds a fresh VM the change takes effect on the next reload. Each
enabled package directory is appended to `package.path`, so a plugin can
`require` its own submodules (the global `lua/lib` wins name collisions), and a
plugin's `themes/` subdirectory is a theme discovery root behind the user's own
`lua/themes/`.

Configuration surfaces expose this uniformly: the `ConfigStore` schema emits one
`plugins` page that flat-lists standalone tools, standalone commands, and plugin
packages, each row carrying its `kind` in a dedicated **Type** column and its true
`tools./commands./plugins.`
enablement path. The old separate `tools` and `commands` pages are gone; a row's
toggle writes to `tools.disabled`, `commands.disabled`, or `plugins.disabled`
accordingly.

## Invariants

- There is one core `Driver` implementation for headless, TUI, and daemon turns.
- The daemon is the authority for mutations; clients request changes through
  protocol commands rather than editing runtime state locally.
- Protocol types are the contract. Add or change a cross-boundary field in
  `protocol` and update every client and test that consumes it.
- A provider response, tool result, approval, cancellation, or view update must
  remain associated with its conversation and turn.
- Cancellation is cooperative: the driver checks it between stream/tool work and
  tools receive the same cancellation path.
- Durable core conversation and runtime writes happen in core/session code, not in a
  frontend renderer. Clients may not write core-owned durable state.

## Documentation source

This file is bundled from `core/defaults/docs/architecture.md` and materialized
under the resolved Bone config directory at startup. Edit the bundled source when
architecture changes; the copy under `.bone-rust/docs/` is generated reference
content and is replaced by the running build.
