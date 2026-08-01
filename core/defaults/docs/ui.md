# Frontends and UI

Bone keeps behavior in core and makes the TUI, web UI, and headless clients thin
frontends. The runtime protocol is authoritative for commands, events,
configuration, sessions, approvals, and view updates.

## Frontend shapes

- The in-process TUI runs a client beside the local daemon and renders the same
  `RuntimeEvent` stream as a remote client.
- `bone serve` hosts the daemon and accepts newline-JSON runtime connections.
  `bone --connect` attaches the TUI to it.
- `bone web` starts the local Node bridge. The browser uses HTTP/SSE; the bridge
  translates requests and event streams to the daemon's runtime protocol.
- Headless `bone run` uses core directly and may emit machine-readable events;
  it has no interactive approval pane or live terminal view.

A browser tab has its own daemon attachment. Clients viewing one conversation
share its actor and event stream; different conversations can run concurrently.
Loading a conversation changes only the requesting client. Approvals and
cancellation are scoped to the attached conversation.

## Command and event boundary

`protocol` is the single source of truth for types crossing the boundary:
`RuntimeCommand`, `RuntimeEvent`, configuration snapshots/actions, session and
tool messages, and `ViewDiff`. A client sends commands and reduces events into
its local rendering state. It must not invent a successful mutation because a
button was clicked; wait for the daemon event/snapshot.

Typical flow:

```text
client command → daemon/session actor → Driver or state mutation
       ▲                                      │
       └──────── RuntimeEvent / snapshot ─────┘
```

Streaming replies, reasoning, tool calls/results, approvals, token state,
conversation loads, and finished/failed turns are all represented by runtime
events. Pair concurrent tool/shell activity by its protocol id, not by arrival
order.

## Rendering and view updates

Rust core and Lua extensions emit declarative view updates. A `ViewDiff` can
upsert or remove a component, set a highlight, or update the theme. Pane content
uses stable source ids, titles, lines, spans, visibility, and scroll state.
Repeated updates for one source replace it; empty content removes it.

The TUI owns terminal layout, wrapping, cursor/input behavior, and color
rendering. The web client maps the same semantic events to browser components and
its document/diff canvas. Neither frontend should duplicate agent-loop,
approval,
configuration, session-persistence, or extension behavior.

Themes are resolved by core and sent as snapshots; clients centralize their
colors through the configured theme. Preserve per-span styling when wrapping,
and keep text content independent from terminal/browser decoration.

## Configuration and session UX

The daemon distributes one revisioned configuration schema and resolved values.
The TUI and web settings views submit typed mutations and render the returned
snapshot. Provider credentials are redacted in frontend snapshots.

The daemon owns core conversation history and active transcript state. A client may
request list/load/new actions and render the resulting snapshot, but clients must not
write core-owned conversation or message tables. The web bridge may persist web-only
metadata, including title and archive state, in `webui_conversations`; it must not
modify core-owned messages or transcript state. On reconnect, restore the selected
conversation by id and request authoritative state rather than replaying guessed local
state.

## Adding a client feature

1. Define or update the cross-boundary type in `protocol`.
2. Implement daemon routing/state changes in `core` and emit the appropriate
   event or snapshot.
3. Update the TUI and web bridge/client to consume the same contract.
4. Add protocol/core tests and exercise the feature through at least one real
   frontend workflow.

Keep client-only preferences local to the client. If a value affects agent
behavior or shared session state, it belongs in daemon-owned configuration or a
runtime command.
