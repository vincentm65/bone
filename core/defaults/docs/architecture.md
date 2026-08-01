# Core Architecture

Bone has one runtime authority: `core`. Frontends are clients of that runtime,
not alternate agent implementations.

```text
TUI / headless runner / web bridge / remote client
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
  extensions, runtime sessions, jobs, and persistence.
- `protocol` owns the serializable commands, events, configuration snapshots,
  session snapshots, tool types, and view types that cross a frontend boundary.
- `tui` owns the native terminal client and its rendering/input code.
- `webui` is a Node/browser client and bridge for `bone serve`; it is not another
  runtime or core conversation-persistence layer. Its bridge may maintain durable
  web-only metadata, such as conversation titles and archived status.

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
or web client sends commands and renders events; it does not own authoritative
transcript, approval, job, or configuration state.

## Data flow

1. A client sends a `RuntimeCommand` through the local or newline-JSON runtime
   connection.
2. The daemon routes it to the conversation actor/session.
3. The session builds a `Driver` for a turn or applies a state mutation.
4. The driver streams provider, tool, approval, view, and status events.
5. The session applies the outcome and persists durable messages and usage.
6. Every attached client receives the same authoritative state/events needed to
   render its view.

Conversation actors may run independently, while clients attached to one
conversation observe its shared actor and event stream. Cancellation and
approvals are conversation-scoped.

## Durable versus ephemeral state

The core-owned SQLite tables are the durable source for conversations, messages, and
runtime records. The web bridge may add durable web-only metadata in
`webui_conversations`; that metadata does not replace or mutate core-owned transcript
state. The model-facing transcript may be a compacted/effective view; display history
remains complete. In-memory driver state, live view components, status text,
cancellation flags, and pending events are ephemeral and must not be treated as
persisted configuration.

Configuration has a separate authority: the daemon's `ConfigStore` loads and
validates the canonical YAML domains, produces one revisioned snapshot, and
broadcasts it to clients. See [Configuration](configuration.md).

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
  frontend renderer. The web bridge may write only its web-only metadata table.
