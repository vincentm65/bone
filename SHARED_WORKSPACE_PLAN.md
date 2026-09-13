# Shared Workspace and Cross-Frontend Plugin Plan

## CEO summary

Build one shared plugin system that works in both the TUI and desktop GUI, while preserving the TUI’s current behavior and user experience.

The desktop will gain a flexible workspace with:

- History pinned on the left.
- A dockable or floating panel area on the right.
- A horizontal dock at the bottom.
- Resizing, moving, stacking, tabbing, hiding, and reordering.
- Saved desktop layouts.

Plugins will provide shared logic, data, commands, and semantic UI descriptions. The TUI and desktop will render those descriptions differently. Plugins will not contain ratatui or egui drawing code.

Existing plugins and the existing TUI pane API remain supported through a compatibility path. New protocol and API features will be additive so the TUI does not need a redesign.

## Goals

1. Make one plugin package usable from both frontends.
2. Keep the current TUI layout, controls, and behavior as the default.
3. Make the desktop workspace flexible enough for VS Code-style panels.
4. Move feature ownership out of hard-coded frontend screens where practical.
5. Support streaming agent and tool output in movable panels.
6. Let plugins receive semantic user actions such as selection, search, submit, and cancel.
7. Keep desktop layout preferences local to the desktop client.
8. Preserve compatibility with existing plugins and older clients where possible.

## Non-goals

- Do not redesign the TUI around the desktop layout.
- Do not expose ratatui, egui, terminal cells, or desktop pixel coordinates to Lua plugins.
- Do not create separate copies of history, agents, themes, or tool logic for each frontend.
- Do not store per-user desktop geometry in shared daemon configuration.
- Do not require every existing plugin to adopt the new API immediately.

## Target architecture

```text
                         Shared daemon/runtime
                 ┌────────────────────────────────┐
                 │ Plugin logic and persistent data│
                 │ Commands, events, permissions   │
                 │ Shared semantic panel state     │
                 └────────────────┬───────────────┘
                                  │ protocol
                    ┌─────────────┴─────────────┐
                    │                           │
             TUI renderer                 Desktop renderer
       Existing layout and keys       Docking, floating, mouse
```

A plugin package owns shared behavior. It may optionally provide different semantic views for different frontends, but those views must still use the shared UI vocabulary. A plugin must not directly draw native widgets.

Example package shape:

```text
lua/plugins/history/
  init.lua              # shared registration and behavior
  ui.lua                # optional semantic panel description
```

If frontend-specific modules are needed later, they must describe content and actions rather than contain toolkit code.

## Current implementation to build on

The repository already has a partial cross-frontend path:

- `protocol/src/view.rs` contains `PaneContent`, `Component`, `ViewModel`, and `ViewDiff`.
- `protocol/src/event.rs` transports view snapshots and diffs.
- `core/src/ext/api_ui.rs` exposes Lua UI operations.
- `core/src/ext/ctx.rs` exposes `ctx.ui.pane`.
- `native/src/state.rs` receives and reduces view updates.
- `native/src/live_pane.rs` renders live content.
- `native/src/panes.rs` renders floating content.
- `tui/src/ui/app/mod.rs` maps incoming panes into the existing pane system.
- `native/src/layout.rs` already persists desktop layout data.

The current gap is that placement and interaction are mostly frontend-specific. The TUI collapses panes into a bottom strip, while the desktop has separate live and overlay paths without a general docking model.

## Detailed implementation phases

### Phase 0: Freeze behavior and define the contract

Before changing implementation code:

1. Record the TUI behaviors that must not change:
   - Existing pane shortcuts.
   - Existing live-pane rendering.
   - Existing pane visibility behavior.
   - Existing plugin loading and reload behavior.
   - Existing key-request behavior.
2. Define the shared concepts:
   - Stable panel ID.
   - Plugin owner.
   - Panel title and optional icon.
   - Panel kind/content.
   - Default slot: left, right, bottom, top, or overlay.
   - Default order and size.
   - Pinned, closable, and collapsible flags.
   - Supported panel actions.
3. Decide which state is shared and which is frontend-local.

Shared state:

- Plugin data.
- Panel identity and content.
- Commands and actions.
- Streaming output.
- Plugin lifecycle.

Frontend-local state:

- Exact position and geometry.
- Dock/floating arrangement.
- Current focus.
- Scroll position where appropriate.
- User layout overrides.

Deliverable: a short protocol/API design note in this file or the extension documentation before implementation begins.

### Phase 1: Add an additive shared panel protocol

Update `protocol/src/view.rs` with optional, backwards-compatible fields and operations:

1. Add a semantic placement type:
   - `PanelSlot::{Left, Right, Bottom, Top, Overlay}`.
   - Stable ordering within a slot.
   - Optional size hint.
   - Pinned and closable flags.
2. Add optional placement and owner data to pane/view components.
3. Preserve old behavior:
   - Existing `Live` panes default to the bottom slot.
   - Existing overlay panes remain overlays.
   - Old plugins without metadata continue to render.
4. Add a placement update operation so a panel can be moved without resending all content.
5. Add serialization and legacy-client tests.

Update `protocol/src/event.rs` with a generic panel action command. It should include:

- Panel ID.
- Action name.
- JSON payload.
- Optional request ID for a reply.

Examples:

```text
history.select { id: "conversation-123" }
agent.cancel { job_id: "..." }
search.submit { query: "..." }
```

Do not send pixel coordinates or terminal-cell rectangles as the shared placement model. Those are frontend-specific.

### Phase 2: Extend the core UI and plugin APIs

Update the shared runtime without changing existing APIs:

- `core/src/ext/api_ui.rs`
  - Extend float/pane options with optional placement metadata.
  - Add placement updates.
  - Preserve existing `open_float`, `set_lines`, `close`, and status-line behavior.
- `core/src/ext/ctx.rs`
  - Allow `ctx.ui.pane` to pass optional panel metadata through unchanged.
- `core/src/ext/ops_events.rs`
  - Add a panel-action event that plugins can handle.
- `core/src/rpc/mod.rs`
  - Route panel actions through the daemon actor loop.
  - Remove panels by plugin owner during reload/disable rather than relying on individual hard-coded IDs.
  - Keep the existing busy-turn and reload behavior.
- `core/src/runtime/view.rs`
  - Apply placement updates and preserve existing validation rules.

The new API must be optional. An old plugin that only emits lines should still work.

### Phase 3: Build the desktop workspace and docking manager

This is the largest frontend change. Keep the existing daemon and TUI behavior intact while replacing the desktop’s fixed pane arrangement.

Update or reorganize:

- `native/src/workspace_ui.rs`
  - Add the workspace regions and docking decisions.
  - Keep the center conversation area stable.
- `native/src/live_pane.rs`
  - Convert the current live pane into a bottom-slot panel adapter.
  - Support multiple live panels, ordering, selection, and visibility.
- `native/src/panes.rs`
  - Treat overlay panels as floating windows.
  - Add docking/floating transitions instead of a single fixed overlay path.
- `native/src/state.rs`
  - Reduce the new placement and panel-action state.
- `native/src/main.rs`
  - Route panel clicks, selections, buttons, close, move, resize, and focus to the appropriate local or daemon action.
  - Preserve existing native actions for jobs and processes until their plugin migration is complete.

Desktop behavior:

1. Reserve a left region for the pinned History panel.
2. Provide a right dock for normal plugin panels.
3. Provide a bottom horizontal dock for live output.
4. Allow any non-permanent panel to move between right, bottom, and floating states.
5. Support tabs or stacks when multiple panels occupy the same region.
6. Allow users to hide and restore panels.
7. Keep panel IDs stable so layout preferences survive reloads.
8. Fall back to a usable default if a plugin is removed or a saved panel is no longer available.

Do not put desktop geometry into daemon configuration. Persist it with the existing native layout mechanism in `native/src/layout.rs`, including a format-version migration.

### Phase 4: Add TUI compatibility support without changing its UX

The TUI should not adopt the desktop docking experience in this phase.

Update only what is needed to consume the additive protocol:

- Map a missing placement to the existing bottom-pane behavior.
- Keep current pane shortcuts and visibility rules.
- Render pinned content in a familiar location.
- Ignore or gracefully collapse unsupported desktop-only features.
- Route generic panel actions through existing key/input behavior where possible.

The existing jobs, processes, queue, and live-pane code should continue to work. Refactoring them into generic adapters is optional and should only happen if it reduces duplication without changing behavior.

### Phase 5: Convert features to shared plugins

Migrate one feature at a time. Each migration must preserve current TUI behavior before adding desktop-specific presentation.

#### History

- Keep conversation/history storage in the existing shared service.
- Move history UI behavior into a plugin or default extension.
- Register a pinned panel with the default left placement.
- Provide actions for search, selection, reopen, and navigation.
- Desktop renders it as a persistent left panel.
- TUI renders it using its current history experience.

#### Agents and subagents

- Keep execution and tool routing in the shared runtime.
- Register live progress and result panels.
- Default to the bottom dock or right dock as appropriate.
- Support cancel, inspect, expand, and focus actions.
- Ensure subagent-scoped panels cannot leak into the parent conversation.

#### Tools and processes

- Preserve existing tool execution behavior.
- Let tools optionally provide a panel and actions.
- Migrate jobs and processes only after the generic path is stable.

#### Themes

Themes are cross-cutting rather than ordinary panels:

- A theme plugin provides semantic color/style tokens.
- The TUI maps tokens to terminal styles.
- The desktop maps tokens to native styles.
- Keep a built-in fallback theme so the application remains usable if a theme plugin is disabled.

#### Plugin management and settings

- Keep install, trust, enable, and disable behavior unchanged.
- Render plugin management through the shared panel system later.
- Removing or disabling a plugin must remove its owned panels and actions cleanly.

### Phase 6: Add richer semantic widgets

Only after the basic panel protocol works, add structured content where needed:

- Text and markdown-like content.
- Lists and selectable rows.
- Trees.
- Tables.
- Buttons and commands.
- Text inputs and search.
- Tabs.
- Progress and status indicators.
- Forms and validation messages.

Each widget needs:

- A semantic representation.
- A stable ID.
- Accessibility/focus information where applicable.
- A list of allowed actions.
- TUI and desktop rendering behavior.

The first implementation should avoid arbitrary custom rendering. A constrained semantic widget set is easier to support consistently in both frontends.

## Compatibility rules

1. Existing Lua plugins remain valid.
2. Existing TUI keybindings remain valid.
3. Existing live panes default to the current TUI behavior and a sensible desktop dock.
4. New protocol fields are optional and have legacy defaults.
5. Unknown optional features must degrade gracefully.
6. Plugin-owned panels are cleaned up on reload and disable.
7. Layout preferences never prevent the application from starting.
8. Plugin code never directly depends on ratatui or egui.

## Testing plan

### Protocol and core tests

- Serialize and deserialize panels with and without placement metadata.
- Confirm old payloads still work.
- Create, update, move, and remove a panel.
- Route a panel action to the owning plugin.
- Remove all panels owned by a plugin on reload/disable.
- Prevent subagent panels from leaking into the parent scope.
- Test reload while a turn is active.

### Desktop tests

- History starts pinned on the left.
- A panel can move right → bottom → floating.
- Panels can be resized, reordered, stacked, hidden, and restored.
- Layout saves and loads correctly.
- Stale panels in saved layouts are ignored.
- Panel actions reach the plugin.
- Multiple conversations keep their panel state separate.

### TUI tests

- Existing unit and integration tests remain green.
- Existing pane shortcuts behave identically.
- Existing plugins render as before.
- Unsupported placement metadata falls back safely.
- Run a real tmux PTY smoke test with an isolated `BONE_DIR`.

### End-to-end test

Create one small test plugin that:

1. Registers a shared command.
2. Opens a panel.
3. Streams updates into it.
4. Accepts a user action.
5. Works in both TUI and desktop.
6. Cleans itself up when disabled.

## Risks and decisions

### Risk: building a second plugin system in the desktop

Avoid this by making the desktop consume the shared panel protocol. Desktop-only behavior should be a renderer or optional semantic view, not a second plugin runtime.

### Risk: protocol changes break old clients

Use optional fields and additive operations. Do not replace existing tagged enum variants with incompatible variants.

### Risk: too much flexibility too early

Start with panels, placement, actions, and a small widget set. Do not begin with arbitrary native widget trees or pixel-level layouts.

### Risk: shared layout expectations

Share panel identity and default placement, but keep exact layout preferences local to each frontend. A desktop arrangement cannot be copied literally to a terminal.

### Risk: history and themes need privileged services

They may be presented by plugins, but storage, conversation access, permissions, and theme application may remain shared core services.

## Recommended order of work

1. Freeze and test current TUI behavior.
2. Define the semantic panel contract.
3. Add backwards-compatible protocol fields and panel actions.
4. Add core plugin panel lifecycle and ownership.
5. Build desktop docking and layout persistence.
6. Add TUI compatibility handling without changing its UX.
7. Migrate one agent/live-panel plugin as the first end-to-end example.
8. Migrate history.
9. Migrate tools, processes, settings, and themes.
10. Add richer widgets only when a real plugin requires them.

## Approval checkpoint

Before implementation begins, approve these decisions:

- The TUI remains behaviorally unchanged.
- New core/protocol features are additive rather than a rewrite.
- The desktop owns exact docking geometry and persistence.
- Plugins provide shared logic and semantic UI, not native toolkit code.
- History is a pinned default panel on the left.
- Right and bottom panels are user-reorganizable.
- Existing plugins continue to work through compatibility behavior.
