# Frontends and UI

Bone keeps behavior in core and makes the TUI, desktop client, and headless clients thin
frontends. The runtime protocol is authoritative for commands, events,
configuration, sessions, approvals, and view updates.

## Frontend shapes

- The in-process TUI runs a client beside the local daemon and renders the same
  `RuntimeEvent` stream as a remote client.
- `bone serve` hosts the daemon and accepts newline-JSON runtime connections.
  `bone --connect` attaches the TUI to it.
- `bone stdio` bridges its stdin/stdout to the loopback daemon (starting a
  detached `bone serve` when none is listening). It is the remote end of a
  client's `ssh <host> -- bone stdio` and writes nothing else to stdout.
- Headless `bone run` uses core directly and may emit machine-readable events;
  it has no interactive approval pane or live terminal view.

Each attached client has its own daemon connection. Clients viewing one conversation
share its actor and event stream; different conversations can run concurrently.
Loading a conversation changes only the requesting client. Approvals and
cancellation are scoped to the attached conversation.

- The first native desktop client is `bone-desktop` in `native/`. It is a thin
  eframe client of a loopback daemon; for a standard loopback address with
  nothing listening it can autostart its own daemon, but custom ports and remote
  addresses never do. Connections are restricted to loopback because the daemon
  protocol has no encryption or authentication. For a remote daemon,
  `bone-desktop --ssh <host>` runs `ssh -T -o BatchMode=yes <host> -- bone stdio`
  (`bone_client::ssh`) and speaks the same stream over the child's stdio; SSH
  owns authentication and host verification, and keys or an agent are required.
  A tab counts as connected only once the daemon's first event arrives, so a
  failed login surfaces ssh's stderr. `BONE_SSH` overrides the `ssh` program and
  `BONE_SSH_REMOTE_BIN` the remote `bone` path. `native` is also a library: an
  embedder can attach `DesktopApp::remote` to a `connection::Target::Custom`
  connector (the Android app's in-app SSH) and gets the same link
  confirmation. Its background Tokio transport reduces
  typed events into a local transcript and never retries a prompt after
  uncertain delivery. Graphics default to wgpu; a returned GPU initialization
  error before app creation triggers one fresh-process retry using OpenGL (glow).
  This never restarts a running app or retries a prompt. Driver panics/aborts are
  not recoverable by this fallback. Set `BONE_DESKTOP_RENDERER=auto|wgpu|glow`
  to choose explicitly; `wgpu` disables the fallback, and `WGPU_ADAPTER_NAME`
  optionally filters presentable adapters by a case-insensitive name substring.
  Startup diagnostics are printed to stderr.
- The desktop mirrors the TUI: a resizable conversation-history sidebar, a tab bar,
  then the transcript, the input, the live pane, and a one-row status bar. Chat
  tabs each own a daemon session; page tabs host the TUI's full-screen pages.
  Sidebar rows mirror `/history`: the title, then a relative timestamp with
  message and token counts; the tooltip carries the full `/history`
  description line (`ConversationMeta.status` / `token_count`). A spinner marks
  a chat whose open tab is mid-turn, and a green dot marks one whose turn
  finished while its tab was not in view (cleared when viewed; desktop-local).
  The ☰ button in the tab strip — and, on narrow windows, in the drawer's own
  header — toggles the sidebar, as does Ctrl/Cmd+B. Windows narrower than 700pt
  start collapsed and open it full-window, closing it once a chat is picked, so
  the same UI works on a phone.
  Ctrl/Cmd+T opens a chat tab, Ctrl/Cmd+W closes a tab (the last chat starts
  over instead), Ctrl/Cmd+Tab or Ctrl/Cmd+PageUp/PageDown cycle, and
  Ctrl/Cmd+1…9 jump. Selecting a history entry focuses the tab showing it,
  reuses an empty chat, or opens a new tab. Right-click a sidebar row to rename
  it or permanently delete it; delete asks for confirmation and closes open tabs
  before removing saved history. Nothing is persisted locally.
  The main area is a grid of panes, each with its own tab strip. Ctrl+\ splits
  right and Ctrl+Shift+\ splits down; Ctrl+Alt+arrows move focus between
  panes. The tab right-click menu offers Split right, Split down and Close.
  Drag a tab to another pane, or onto a pane edge to split, to move it; drag
  dividers to resize. A narrow window has no room for side-by-side panes, so it
  always stacks: any split becomes a full-width pane above another, and its tab
  strip's ⋮ menu offers Split below and Close tab, because a touch screen has no
  right-click.
  A new chat, including one created by `/new`, remains ephemeral until its first
  real prompt is submitted; empty chats are not written to SQLite and do not
  appear in history or the sidebar. Persistence and lifecycle remain daemon-owned.
  Sidebar actions are limited to `Rename…` and `Delete…`; archive is deferred.
- Transcript, live pane, status bar, and pages are drawn by the shared
  `bone-render` crate exactly as the TUI draws them, then painted on a character
  cell grid, so Markdown, tool rows and previews, colors, and wrapping match the
  terminal. Clicking a tool row expands it; Ctrl/Cmd+O expands every row.
- Command replies, shell output, and tool rows are plain text. `bone-render` strips
  ANSI/VT escape sequences (SGR attributes, OSC/DCS strings, other C0 bytes and
  DEL, while keeping `\n` and `\t`) before Markdown or row rendering, so escapes
  are ignored rather than interpreted and both frontends show the same characters.
  A styled `/help` banner or a Lua command's `\x1b[..m` labels render as their
  plain text instead of raw escape bytes.
- The desktop embeds its UI fonts and installs them ahead of egui's own faces:
  Inter (proportional), Inter SemiBold (the `semibold` family), and JetBrains Mono
  (monospace), with Adwaita Mono appended to both chains as the coverage fallback
  for symbol ranges such as braille spinner frames and box drawing. Adwaita Mono
  is inserted before egui's built-in monospace faces because it shares their
  advance width, so fallback glyphs stay on the terminal cell grid. Bundled
  spinner presets and other core UI text use only glyphs this set covers;
  configured CJK fallbacks still come from the system and are tofu when none is
  installed.
- `/stats`, `/setup`, and `/catalog` open page tabs running the TUI's screens
  with the same keys; `/catalog install|remove NAME` applies directly. Clicking
  an agent or process in the live pane (or focusing one with plain Down from
  exact empty live input and pressing Enter with trimmed-empty input) opens its
  transcript or live-output viewer in a page tab; Ctrl+C in a process viewer
  cancels the process.
- The live pane shows one page at a time: daemon panes (Lua panes and menus such
  as `/config` and `/provider`), the approval prompt, agents, processes, the
  queue, and live reasoning when `general.show_reasoning` is on. The desktop's
  reasoning page is a fixed 10 rows (wrapped, newest text at the bottom) and stays
  up between segments until the turn ends. A newly arrived
  page becomes active, Tab cycles pages, and PageUp/PageDown scroll. The queue
  page takes ↑/↓, Shift+↑/↓ to reorder, Enter to send next, F2 to edit, and Del
  to remove. While a `ctx.ui.key()` request is pending every key, including Esc
  and Tab, goes to the daemon; `toggle_panes` hides the pane.
- Approvals use the TUI prompt: Accept, Advise, or Cancel with ↑/↓ and Enter,
  P for the full command, Esc to cancel. Advise takes typed advice in the input
  and returns it to the model as the tool result; Cancel denies and stops the
  turn. Running shell commands appear above the input with elapsed time.
- The input uses `ui.input.prefix` (default `> `). Enter sends (queues while
  busy), Ctrl/Cmd+Enter steers a running turn, Shift+Enter inserts a line,
  Ctrl/Cmd+Up/Down recalls history, Ctrl/Cmd+D clears the queue, Alt+V attaches
  the clipboard image, and Esc stops the running turn. Plain Up/Down recall
  older/newer submitted prompts; Down from exact empty live input focuses the
  first active agent/process, then plain Up/Down move a clamped selection, and
  Up from the first row returns to empty live input. Enter opens the focused
  agent/process only when `composer.trim().is_empty()`; autocomplete keeps plain
  Up/Down for its own selection, and ordinary editing clears list focus. `/edit`
  opens the draft in `$VISUAL`/`$EDITOR` (terminal editors in `$TERMINAL` or a
  detected terminal). Configured `keymaps.bindings` take precedence.
- Touch and mouse: pane lines and spans may carry a `click` value (`ui.menu`
  sets option indexes and tappable hints, `/config` its tabs, rows, and provider
  editor action); tapping one answers the pending key request with
  `KeyEvent { code: "Click", char }`. When the `/config` tab row would not fit
  the pane width (phones, very narrow panes), it collapses to one tappable
  section header that opens a section picker; a row that fits is unchanged. A
  page line wider than the pane still pans horizontally with the mouse wheel,
  Shift+wheel, or a touch swipe; taps map through the pan offset.
  Approval choices are tappable. On touch screens a Stop button ends the input
  row while a turn runs, Android Back acts as Esc, and the on-screen keyboard
  stays up while a menu takes keys. The tab strip's ☰ toggle opens the
  conversation drawer without a keyboard, and its ⋮ menu splits stacked panes or
  closes the tab where a right-click cannot reach. The composer frame follows
  `ui.input.preset` (`lines`, `box`, `filled`) with its padding and `fill`.

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
rendering. On non-Windows terminals, resizing the inline viewport first resets
ANSI scrolling margins, then clears at the tracked viewport top before rebuilding.
This lets newline-based allocation scroll transcript rows out of a growing pane's
way even if a restricted scrolling region was active. The desktop client paints
the same `bone-render` output on its character grid. Neither
frontend should duplicate agent-loop, approval, configuration, session-persistence,
or extension behavior.

## Shared plugin panels

Plugin UI is frontend-neutral. Plugins describe panes and floats through the
shared view protocol; they do not import ratatui, egui, or any other renderer.
Each panel has a stable component id and may optionally declare semantic
placement (`left`, `right`, `top`, `bottom`, or `overlay`), ordering, a size
hint, pinned/closable state, and an owner. Existing Lua plugins that omit
placement continue to use the legacy frontend defaults.

Panel placement is not daemon-wide geometry configuration. It is semantic data
that any frontend may interpret. The TUI and desktop deliberately preserve the
bottom live-pane layout and treats placement-only updates as no-ops, so old
plugins and their APIs retain their current behavior. Panel actions travel back
through the additive `RuntimeCommand::PanelAction` command and are delivered to
the daemon's managed `panel_action` hook; clients do not execute plugin logic
locally. When a plugin is reloaded or disabled, the daemon removes components it
owns while preserving owner-less legacy components.

Themes are resolved by core and sent as snapshots; clients centralize their
colors through the configured theme. Preserve per-span styling when wrapping,
and keep text content independent from terminal/browser decoration.

## Configuration and session UX

The daemon distributes one revisioned configuration schema and resolved values.
The settings views submit typed mutations and render the returned
snapshot. Provider credentials are redacted in frontend snapshots.

Stats, catalog, and setup share the correlated daemon-host API. Local and remote
clients render the same data and submit the same plans; SQLite queries, catalog
downloads, credentials, and setup files remain on the daemon host. Provider
setup is offered automatically only for a genuinely unconfigured install; a
restored provider credential suppresses unsolicited startup onboarding.

The TUI's `/catalog` lists the catalog's `kind = "plugin"` packages with
install, update, remove, and enable/disable controls. Enable/disable is
plugin-level: a disabled plugin stays installed but none of its capabilities
(tools, commands) register. The TUI reaches the same plugin install/update/remove
through `/catalog`. The config UI presents a single **Plugins** page that unifies
every plugin package with the built-in capabilities that no plugin owns in one
flat list — each row carrying its `tool` / `command` / `plugin` type in a
dedicated **Type** column (blank for any row without one). Enabling or disabling
any row routes to the matching canonical setting (`tools.disabled` /
`commands.disabled` / `plugins.disabled`). The Bone-owned `lua/core` package is
absent from all of these: it is always loaded, so it has no catalog entry and no
enable/disable row, and a `plugins.core` value is rejected. Installing or
updating a plugin asks for explicit consent first — Bone Lua is not sandboxed
and runs with the user's authority — and declining leaves the plugin tree
unchanged. Every file's `sha256` is verified before anything is written.

The daemon owns core conversation history and active transcript state. A client may
request list/load/new actions and render the resulting snapshot, but clients must not
write core-owned conversation or message tables. Client-only metadata must not
modify core-owned messages or transcript state. On reconnect, restore the selected
conversation by id and request authoritative state rather than replaying guessed local
state.

The TUI bounds how many rendered rows it keeps in memory via `ui.history_rows`
(default 3000, 0 = keep all). Between turns, rows already flushed to terminal
scrollback beyond the cap are replaced by one marker row, and conversation loads
request only the newest window. This is client-side display state: the daemon's
SQLite store and the model-facing context keep the full history. Inline tool
rows collapse long shell output to its head and tail, and tool labels such as
long shell commands to their first nine lines, each with a `⋮ +N … (ctrl+o)`
marker. Ctrl+O opens the paged transcript viewer, which shows commands and
output in full and fetches older pages on demand.

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
