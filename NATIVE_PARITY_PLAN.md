# Native TUI-Parity Implementation Plan

Tracking doc for bringing the missing TUI features into the native Rust UI
(`native/`, package/binary `bone-desktop`).

## Objective

Implement the missing TUI features in native **without changing tool Lua,
protocol schemas, or command definitions**. The daemon stays the sole authority;
native depends only on `bone-protocol` + `bone-client` + Tokio + GUI libs (never
`bone-core`) and does not run Lua or the DB.

## Verdict

Feasible. The protocol already exposes every needed wire message. Work is almost
entirely in `native/`. Two features (`/update`, `/edit`) are client-local and
need no protocol. A few wire fields are opaque JSON, so native mirrors small
view structs (duplication, not schema change).

## Scope decisions (approved)

- `/update` and `:`/`!` inline shell are **client-local** (match TUI behavior).
- `ToolDisplayConfig`, `ThemeSettings`, and built-in command metadata are
  **mirrored in native** (avoids moving types into `protocol`), with round-trip
  fixture tests.

## Protocol surface used (no schema changes)

- `RuntimeCommand::{RunCommand, KeyReply, GetProcesses, GetJobs, CancelProcess,
  CancelJob, SetConfigValue, ResetConfigValue, SetToolEnabled, SetCommandEnabled,
  SetIncognito, SetApprovalMode, ReloadExtensions, ReloadSettings, AppendMessage,
  ReplaceConversation, ClearConversation}`.
- `RuntimeEvent::{ViewSnapshot, ViewDiff, ProcessesSnapshot, JobsSnapshot,
  TokenUsage, CommandComplete, FrontendState, ConfigSnapshot, ConfigChanged,
  ConfigMutationRejected, KeymapDispatched, WorkElapsed}`.
- `HostRequest::{Stats, Conversations, Catalog, CatalogApply, Setup, SetupApply}`.
- `protocol::{view, config, input, tools}` types.

## Phases

- [x] **Phase 0 — Foundations** (`native/src/state.rs`, `native/src/main.rs`)
  - [x] Extend reducer + per-tab state for `TokenUsage`
  - [x] Reduce `ViewSnapshot` / `ViewDiff`
  - [x] Reduce `ProcessesSnapshot` / `JobsSnapshot`
  - [x] Reduce `CommandComplete`
  - [x] Reduce full `FrontendState` (commands/settings/tool_defs/tool_display)
  - [x] Command-dispatch path + request-id correlation
  - [x] egui-key → `KeyEvent.code` mapping (crossterm names)
  - [x] Slash detection + autocomplete
- [x] **Phase 1 — Slash commands** via `RunCommand`/`CommandComplete`; apply
  `CommandAction`; `/help`, `/clear`/`/new`, `/model`, `/provider`
- [x] **Phase 2 — Config & toggles** — schema-driven page from `GetConfig`;
  `SetConfigValue`/`ResetConfigValue`; `SetToolEnabled`/`SetCommandEnabled`;
  `SetApprovalMode`; `SetIncognito`; theme via `ViewDiff::SetTheme`
- [x] **Phase 3 — Declarative panes** — reduce/render `ViewSnapshot`/`ViewDiff`
  (`Component::Float`/`StatusLine`, highlights)
- [x] **Phase 4 — Interactive keys** — `KeyRequest` → `KeyReply` capture modal
- [x] **Phase 5 — Processes/jobs** — `GetProcesses`/`GetJobs`,
  `CancelProcess`/`CancelJob`, job transcript from `JobSnapshot.events`
- [x] **Phase 6 — Stats & catalog** — `HostRequest::Stats`,
  `HostRequest::Catalog`/`CatalogApply`, `catalog_updates` badge
- [x] **Phase 7 — Custom tool display** — mirror `ToolDisplayConfig` from
  `FrontendState.tool_display`; generic fallback
- [x] **Phase 8 — Client-local** — `/update`, `/edit`, `:`/`!` inline shell +
  `AppendMessage`, message queue, input shortcuts

## Known tradeoffs

- Mirror `ToolDisplayConfig`, `ThemeSettings`, and built-in command metadata
  (`BUILTINS` are not in `FrontendState.commands`); keep mirrors small with
  round-trip fixture tests.
- `/update` and inline shell act on the client machine for remote daemons
  (matches TUI).
- `/update` uses the same client-local approach as TUI (`open_update` at
  `tui/src/ui/app/mod.rs`); no host API exists for updates.

## Open items

- egui → `KeyEvent.code` mapping table (unit test) — resolved during Phase 4.

## Validation per phase

- Native reducer + ux unit tests.
- Fixture-daemon integration (`native/test-support/mock_provider.py`).
- GUI smoke test with an isolated temporary `BONE_DIR` (Hyprland `grim` capture
  or xvfb).
- Cross-check identical slash commands against one daemon in TUI and native.

## Progress log

- 2026-09-08: Plan documented. Starting Phase 0.
- 2026-09-08: Phase 0 complete. Reducer arms (TokenUsage/WorkElapsed/ViewSnapshot/
  ViewDiff/Processes/Jobs/CommandComplete/FrontendState); request-id correlation;
  egui-key→`KeyEvent.code` (`keys.rs`); slash autocomplete (`commands.rs`).
  `cargo check -p bone-desktop` clean; unit tests green.
- 2026-09-08: Phase 1 complete. `submit_composer` routes slash vs prompt;
  `apply_command_complete` correlates by id, renders by `display_role`, echoes +
  busy on `submit`; `apply_command_action` forwards `conversation_replace`/
  `conversation_load` and applies `config_action`. 4 new tests; full bin suite
  129 passed / 0 failed.
- 2026-09-08: Phase 2 complete.
  - `native/src/theme.rs` mirrors `ThemeSettings`/`Palette` (serde, no schema
    change) with `parse_color` (`#rgb`/`#rrggbb`/`#rrggbbaa`) and
    `ThemeSettings::visuals()` mapping the resolved palette onto egui `Visuals`
    (dark/light base by luminance).
  - `native/src/config_view.rs` renders the daemon `ConfigSchema`/`ConfigSnapshot`
    as an egui dialog with set/reset; `tools.<name>`/`commands.<name>` render as
    enablement toggles driven by the snapshot disabled lists (not `values`).
  - `SetToolEnabled`/`SetCommandEnabled`, `SetApprovalMode`/`SetIncognito`
    (toolbar Safe/Danger + Incognito), and `ViewDiff::SetTheme` → `ctx.set_visuals`
    (only reapplied when the payload changes). `serde` added as a direct dep.
  - Full bin suite 140 passed / 0 failed / 1 ignored; `cargo check --workspace`
    clean (9 benign warnings).
- 2026-09-08: Phase 3 complete.
  - New `native/src/panes.rs` renders the daemon `ViewModel`: `Component::Float`
    as an anchored egui `Area` (anchor→`Align2`, `col`/`row`→cell offset, width/
    height→point caps, `border` toggles the frame, `scroll` skips rows) and
    `Component::StatusLine` as a left/center/right colored row.
  - `resolve_color` resolves `#rgb`/`#rrggbb`/`#rrggbbaa`, palette role names
    (`fg`/`accent`/…), and `ViewModel.highlights` names; spans honor
    bold/dim/italic/strike modifiers and line `bg`.
  - Wired into `conversation_pane` (floats, salted by tab id) and the toolbar
    per-tab status row (status lines), colored by `DesktopApp::palette()`.
  - 2 new tests (color resolution + headless render smoke); full bin suite
    142 passed / 0 failed / 1 ignored; `cargo check --workspace` clean.
- 2026-09-08: Phase 4 complete. `KeyRequest` sets `State.pending_key`; a modal
  `key_capture_dialog` consumes the next key press, maps it through `keys::key_event`,
  and replies with `RuntimeCommand::KeyReply`; shortcut handling is suspended while a
  capture is pending. 2 new tests; full bin suite 144 passed / 0 failed / 1 ignored.
- 2026-09-08: Phase 5 complete.
  - New `native/src/activity.rs` renders the daemon `ProcessSnapshot`/`JobSnapshot`
    lists inside the Activity window, mirroring the TUI's `processes_pane`/
    `jobs_pane`/`process_view`: process command/state/elapsed + output tail, job
    header (icon/agent/title), status/provider/elapsed/token totals, and a
    collapsible transcript from `JobSnapshot.events` (TextDelta/ReasoningDelta/
    ToolCall/ToolResult/Failed). Pure helpers (`format_tokens`, `format_elapsed_ms`,
    `process_state_label`, `job_status_icon`) mirror the TUI; cancel buttons emit
    `ActivityAction`s the caller turns into `CancelProcess`/`CancelJob`.
  - `DesktopApp::activity_dialog` (gated `!demo && show_activity`) renders the first
    connected tab's snapshots; a toolbar Activity button (with a count badge) toggles
    it and sends `GetProcesses`/`GetJobs` for an immediate first snapshot.
  - 5 new tests (activity helpers/render + `refresh_activity` + dialog render smoke);
    full bin suite 152 passed / 0 failed / 1 ignored; `cargo check --workspace` clean.
- 2026-09-08: Phase 6 complete.
  - New `native/src/stats.rs` renders the daemon `UsageStatsSnapshot` in a "Token
    stats" window: range selector (Today/7d/4w/Yearly/All), summary cards
    (requests/prompt/completion/cached/total/cache%), a newest-first usage bar
    chart, and a provider/model breakdown. Pure helpers (`compact_number`,
    `cache_percent`, `bucket_tokens`, `mode_title`) mirror the TUI; a refresh
    button emits `StatsAction::Refresh`.
  - New `native/src/catalog.rs` renders the daemon `CatalogSnapshot` in a
    "Catalog" window grouped Updates/Installed/Available with per-item
    install/update/remove buttons; `action_message`/`applied_summary` mirror the
    TUI's `catalog_action_message`.
  - `DesktopApp` gains `refresh_stats`/`request_catalog`/`apply_catalog_actions`
    senders (request-id correlated like `Conversations`/`Setup`), `apply_stats_
    response`/`apply_catalog_response` handlers, stale-slot cleanup in
    `drain_all`, and `stats_dialog`/`catalog_dialog`. Toolbar Stats + Catalog
    (with `catalog_updates` badge) buttons toggle them; opening Catalog sends
    `HostRequest::Catalog { refresh: true }`.
  - 10 new tests (senders/handlers/`catalog_updates` + headless dialog render);
    full bin suite 168 passed / 0 failed / 1 ignored; `cargo check --workspace`
    clean (6 benign warnings).
- 2026-09-08: Phase 7 complete.
  - New `native/src/tool_display.rs` mirrors core `ToolDisplayConfig` (core-local,
    not in `bone-protocol`) and ports the TUI's `tool_label`/template/`args`/
    `value_labels`/shell/heredoc/read-file-summary rendering. `parse_map` turns
    the opaque `FrontendState.tool_display` payload into a name→config map
    (malformed → empty). `custom_label` returns `None` when a tool has no config
    (generic fallback), `Some("")` when `show = false` (hide heading).
  - `State` stores the parsed map, computes `ToolCard.label`/`show_result` on
    `ToolCall`/`ToolResult`/history replay, and `refresh_tool_labels()` recomputes
    existing cards when `FrontendState` arrives after a replayed conversation.
  - `transcript::render_tool_row` prefers `card.label` over the generic name
    (empty label hides the heading) and skips the result body when
    `show_result = false`, keeping toggle/state/count/Copy/args.
  - `eager` is mirrored but not acted on: native always creates the row at
    `ToolCall`, so there is no streaming-deferral behavior to gate.
  - 10 new tests (8 in `tool_display.rs` + 2 reducer tests in `state.rs`); full
    bin suite 178 passed / 0 failed / 1 ignored; `cargo check --workspace` clean
    (6 benign warnings); `rustfmt --check` clean on the new file.
- 2026-09-08: Phase 8 complete.
  - Client-local features live in `native/src/local.rs` and are routed in
    `submit_composer` before `can_send`, matching the TUI: `/update` (client-side
    update check), the `/edit` modal, and `:`/`!` inline shell that runs on the
    client and folds its output into the conversation via `AppendMessage`.
  - Message queue: `Tab::enqueue_composer` pushes the trimmed draft while busy,
    `drain_queue` sends one queued prompt per idle frame (guarding on empty
    composer/attachments), the composer shows a bounded `Queued (N)` list with
    per-item remove and Ctrl/Cmd+D clear, and `ConversationLoaded` drops the
    queue while `/clear`/`/new` snapshots preserve it.
  - Input shortcuts: Ctrl/Cmd+Up/Down recall/dedupe a capped (500-entry)
    `Tab.history`, Esc clears the draft (skipped while autocomplete is open),
    and the composer records history on a successful send. Ctrl+U/Ctrl+W are
    left to egui's `TextEdit`.
  - 11 new tests (4 queue + 3 history/shortcut + 4 `/update`/`/edit`/shell); full
    bin suite 196 passed / 0 failed / 1 ignored; `cargo check --workspace` clean
    (6 benign warnings).
