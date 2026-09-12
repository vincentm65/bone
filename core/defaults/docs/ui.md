# Frontends and UI

Bone keeps behavior in core and makes the TUI, desktop client, and headless clients thin
frontends. The runtime protocol is authoritative for commands, events,
configuration, sessions, approvals, and view updates.

## Frontend shapes

- The in-process TUI runs a client beside the local daemon and renders the same
  `RuntimeEvent` stream as a remote client.
- `bone serve` hosts the daemon and accepts newline-JSON runtime connections.
  `bone --connect` attaches the TUI to it.
- Headless `bone run` uses core directly and may emit machine-readable events;
  it has no interactive approval pane or live terminal view.

Each attached client has its own daemon connection. Clients viewing one conversation
share its actor and event stream; different conversations can run concurrently.
Loading a conversation changes only the requesting client. Approvals and
cancellation are scoped to the attached conversation.

- The first native desktop client is `bone-desktop` in `native/`. It is a thin
  eframe/wgpu client of a loopback daemon; for a standard loopback address with
  nothing listening it can autostart its own daemon, but custom ports and remote
  addresses never do. Connections are restricted to loopback because the daemon
  protocol has no encryption or authentication; remote use requires an SSH
  tunnel to a forwarded `127.0.0.1` port. Its background Tokio transport reduces
  typed events into a local transcript and never retries a prompt after
  uncertain delivery.
- Native supports multiple conversation tabs arranged in a frontend-only, Zed-style
  recursive split pane tree (each leaf pane holds one conversation), streamed
  Markdown text with images, multiline prompts, cancellation,
  approve/deny controls, provider setup and settings dialogs, and keyboard
  shortcuts. The sidebar groups tasks under Open and Recent, without duplicating
  open conversations in Recent. Recent rows show relative UTC dates with full
  timestamps on hover; an unattached tab labels itself from its composer draft.
  Client-only display preferences (zoom, pane widths) persist in the
  desktop state file, not daemon configuration. When the OS permits, native
  window sizes and positions persist as well; the split tree (ratios, pane
  order, active pane, and composer drafts) persists in the desktop state file,
  with legacy single-split layouts migrating to the tree. The sidebar defaults to about
  20% of the window width (with readable minimum/maximum bounds), can be
  dragged wider or narrower, and a manual width is restored across restarts;
  legacy layouts migrate to the proportional default until the user resizes.
  Right-clicking any open or recent task row provides its task actions without
  adding per-row buttons. Both lists support rename/delete and title search;
  open rows also retain a close action, and selecting an open row focuses its
  owning tab and raises its window. Renames update the open
  task title. Deleting an open task explicitly confirms stopping work and
  discarding its draft, closes its connection, then deletes the saved history.
  The desktop toolbar shows connection, task/workspace identity, sidebar and
  split controls, a unified Tools menu for local tool-call display and
  server-scoped tool permissions. The model selector lives in the compact
  growing composer footer. Secondary actions (incognito, activity, stats, catalog,
  extensions, dialogs) sit in one menu, and the layout is
  responsive: the central transcript always keeps a minimum width, so in a
  narrow window the split tree is retained but a single branch is focused and
  the sidebar is hidden (both return when the window widens); an overflowing
  tab row exposes a Focus pane action.
  The composer shows either Send or Stop, never both; empty ready conversations show a calm start prompt, and load failures present brief recovery actions with full technical details behind a disclosure rather than duplicating raw daemon errors across the chrome and transcript. Permission/privacy mutation feedback is surfaced in the main toolbar without adding transcript clutter.

- Panes split recursively: Ctrl+\ splits the focused pane to the right and
  Ctrl+Shift+\ below. Each pane holds a single row of tabs that scrolls
  horizontally when it overflows, with a New tab action, per-tab close,
  reordering, and a context menu. Splitting, tab state, and window layout are
  frontend-only; none of them create or reconnect daemon sessions.
- Tab shortcuts are pane-local: Ctrl+1..9, PageUp/PageDown, Ctrl+T (new), and
  Ctrl+W (close) act only on the focused pane's tab group. Ctrl+Shift+N opens a
  new native window. The layout menu and tab context menus can move a tab or
  its whole pane between native windows without reconnecting its session.
- Closing a tab confirms first when its conversation is busy, holds an unsent
  draft, or has attachments, and never deletes saved history. Closing a
  secondary window rehomes its tabs into the main window; closing the main
  window is the quit path, saving desktop state and confirming that unsent
  composer images are discarded, not saved.

- Native task switching uses Ctrl/Cmd+P; Ctrl/Cmd+K opens a searchable command
  picker. Arrows and Enter select, Escape dismisses. App actions such as settings
  and stats open directly; other commands use a separate argument dialog, keeping
  the composer draft intact. Escape preserves drafts and attachments, and
  Copy/Cut retain desktop text-editing behavior. `/edit` opens the draft in a larger
  editor; clearing the composer still exposes Undo. Incognito
  targets the selected task, with persistent sidebar and composer indicators.
  Model selection offers configured and previously used models plus custom IDs.
  Host API 3 adds `SetConversationModel`, which selects and persists a model for
  one conversation without changing shared provider defaults; incognito choices
  stay in memory. Older servers can still switch among configured providers.
  Shared provider-default editing is a separate, explicitly labeled section.
  Transcript and composer columns share an 800-logical-pixel cap and centered
  side insets; wide code and tables scroll horizontally. Bundled Inter regular
  and semibold give prose a real weight hierarchy, with JetBrains Mono for code
  and egui's fallback fonts retained for symbols. Conversation text uses a 16px
  face with 24px lines, compact section spacing, unboxed assistant answers, and
  content-sized short prompts inside right-aligned, lightly rounded surfaces.
  Code uses an integrated language/Copy header; tables and rules have quiet
  outlines. Small corner radii and tighter padding give chat an editor-like
  density. The flat, subtly outlined composer keeps the model selector secondary to
  Send/Stop, while preserving configured input presets, prefix, and padding.
  Tools → Concise (default) or Verbose controls tool-call detail, remembered
  across restarts and applied to every conversation. Concise mode shows one-line
  labels with file paths or shell commands; raw arguments and shell output stay
  behind the disclosure. Verbose mode expands those details. Successful calls
  omit Done labels; failures remain visible. Individual expansion choices survive
  live updates until the display mode changes. Copy always includes full output.
  Applied edit_file results show numbered red/green diffs in both modes, with
  more lines available on demand. These diffs come from the daemon's actual tool
  result, including in reopened conversations. Running calls retain a spinner.
  Tool errors soft-wrap without changing their raw text or newlines; arguments
  and other preformatted output retain horizontal scrolling.
  Approval controls precede the scrollable summary and preview, and their row
  wraps in narrow panes. Each newly targeted approval starts at the top so its
  Approve/Deny controls are immediately reachable.
  Reopened conversations and synchronization keep results inside their original
  tool cards, preserving arguments, error states, and configured presentation;
  results from a bounded history suffix still get cards when the call is absent.
  These are desktop presentation choices, not changes to tool or approval behavior.
- A collapsible live pane sits above each conversation's composer. Its pages
  show active Agents and daemon-provided task lists or extension content, with
  independent selection, scrolling, and follow state in each conversation. Agent
  rows reuse the task-row styling, show inline status and elapsed time, and open a
  live job transcript; cancellation targets the conversation that launched the
  viewer. Task lists retain daemon-owned styling and remain until removed.
  The pane defaults to one-third of the conversation height and can be resized
  within a bounded range; following pages tail new output, while pages read away
  from the latest output show a new-output/jump-to-latest affordance. Explicit
  Lua floats retain overlay placement. Pane visibility and navigation shortcuts
  apply to live pages and overlays.
- Native interactive key requests display the owning task's daemon-provided
  float pane titles and styled lines inside a bounded, scrollable input modal,
  including when that task is in the background or panes are hidden. Menu
  questions, options, previews, and selection remain Lua-owned; native forwards
  keys through `KeyReply` before app shortcuts and prevents captured input from
  reaching the composer. Requests without pane content keep a generic key prompt.
- Recognized local file references prompt before opening Zed or VS Code via
  their registered URL handler. Paths must remain inside the pane's daemon
  workspace, which must also exist locally; remote path mapping is not provided.
- Workspace changes (toolbar Changes action) requires host API v2 or newer. The daemon returns bounded,
  read-only Git status and staged/unstaged diffs for its workspace, not task-only
  changes. The snapshot is non-atomic, untracked contents are omitted, truncation
  is explicit, and no stage/revert/commit actions are offered. A selectable file
  list shows descriptive statuses and separate staged/unstaged diffs per file.
  Narrow windows stack the file list above the diff; Copy includes the full
  received diff for that file even when the rendered view is capped.

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

On attach, core sends a complete `ViewSnapshot` in addition to the normal
session snapshot. A client replaces its local component and highlight model
with that full view; it must not merge it like a diff. When repairing a lagged
event stream, the correlated `StateSynchronized` event contains the full view
itself, making the view and completion one atomic frame. Older clients may skip
the additive snapshot field and continue consuming the unchanged session data.

An attachment replay sends `ConversationLoaded` before `ViewSnapshot`, so the
conversation reset clears old transient state before the full daemon view is
installed. To stay within the newline-JSON frame limit, an oversized replay
keeps the newest transcript suffix rather than severing the attachment. A
synchronization repair applies the full view from its correlated
`StateSynchronized` frame, then core replays pending approval/key interactions.
This both makes repair atomic and preserves a recovered prompt after the reset.
In-process conversation loads use the same reset-then-view ordering, and an
extension reload sends its new full view after `FrontendState`; an empty view
explicitly removes UI owned by the replaced extension runtime.

When an attachment's `ConversationLoaded` snapshot is already busy, clients
join the existing turn without adding another user message and immediately
start correlated synchronization. They keep repairing until the actor is idle,
which restores any stream head missed before attachment and replays an
outstanding approval/key gate. Replayed interaction ids are idempotent in the
clients, so one lagging attachment cannot create duplicate prompts elsewhere.
Approval and key-request IDs are separate namespaces; clients must track answered
IDs separately so an approval cannot suppress a tool's keyboard input request.

The TUI owns terminal layout, wrapping, cursor/input behavior, and color
rendering. The desktop client maps the same semantic events to its eframe
components and its document/diff canvas. Neither frontend should duplicate
agent-loop, approval, configuration, session-persistence, or extension behavior.

Themes are resolved by core and sent as snapshots; clients centralize their
colors through the configured theme. Preserve per-span styling when wrapping,
and keep text content independent from terminal/browser decoration.

## Configuration and session UX

The daemon distributes one revisioned configuration schema and resolved values.
The TUI and desktop settings views submit typed mutations and render the returned
snapshot. Provider credentials are redacted in frontend snapshots.

Stats, catalog, and setup share the correlated daemon-host API. Local and remote
clients render the same data and submit the same plans; SQLite queries, catalog
downloads, credentials, and setup files remain on the daemon host. Provider
setup is offered automatically only for a genuinely unconfigured install; a
restored provider credential suppresses unsolicited startup onboarding.

The desktop Extensions screen lists the catalog's `kind = "plugin"` packages with
install, update, remove, and enable/disable controls. Enable/disable is
plugin-level: a disabled plugin stays installed but none of its capabilities
(tools, commands) register. The TUI reaches the same plugin install/update/remove
through `/catalog`. The config UI presents a single **Plugins** page that unifies
standalone tools, standalone commands, and plugin packages in one flat list — each
row carrying its `tool` / `command` / `plugin` type in a dedicated **Type** column
(blank for any row without one) — so a plugin, a plain-file
tool, and a plain-file command are managed the same way. Enabling or disabling any
row routes to the matching canonical setting (`tools.disabled` / `commands.disabled`
/ `plugins.disabled`). Installing or updating a plugin asks for explicit consent
first — Bone Lua is not sandboxed and runs with the user's authority — and declining
leaves the plugin tree unchanged. Every file's `sha256` is verified before anything
is written.

The daemon owns core conversation history and active transcript state. A client may
request list/load/new actions and render the resulting snapshot, but clients must not
write core-owned conversation or message tables. Client-only metadata must not
modify core-owned messages or transcript state. On reconnect, restore the selected
conversation by id and request authoritative state rather than replaying guessed local
state.

## Adding a client feature

1. Define or update the cross-boundary type in `protocol`.
2. Implement daemon routing/state changes in `core` and emit the appropriate
   event or snapshot.
3. Update the TUI and other clients to consume the same contract.
4. Add protocol/core tests and exercise the feature through at least one real
   frontend workflow.

Keep client-only preferences local to the client. If a value affects agent
behavior or shared session state, it belongs in daemon-owned configuration or a
runtime command.
