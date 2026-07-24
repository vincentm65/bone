# Background Activity UI Refactor Plan

## Goal

Remove duplicated Agents-pane and Processes-pane UI behavior while preserving their current user experience:

- Up/Down selects an active item.
- Enter opens the selected item when chat input is empty.
- Enter falls through to message submission when chat input is nonempty.
- Unmodified `k` requests cancellation.
- Selection highlighting and scrolling remain identical.
- Idle and streaming input paths behave consistently.

Keep subagent and process execution, registries, cancellation mechanisms, and viewers separate.

## Principles

- Share UI mechanics, not unrelated runtime semantics.
- Preserve current ordering, labels, styles, key behavior, and pane visibility.
- Use stable IDs for selection; never store selection by row index.
- Keep backend-specific data conversion explicit and small.
- Avoid a generic registry or background-task runtime abstraction.
- Prefer fewer branches and types over a trait-heavy framework.
- Preserve all existing uncommitted changes during implementation.

## Current Duplication

### Navigation

`tui/src/ui/app/mod.rs` contains parallel `AgentsKeyResult` and `ProcessesKeyResult` types and nearly identical navigation functions. Both implementations:

- reject modified keys;
- find the selected ID in an active snapshot;
- clamp Up/Down movement;
- open on Enter only when input is empty; and
- return the selected ID for `k` cancellation.

### Rendering

`tui/src/ui/jobs_pane.rs::render_selected` and `tui/src/ui/processes_pane.rs::render` independently implement:

- active-item filtering;
- selected-row markers;
- the same selected background;
- eight visible rows;
- selected-index lookup; and
- selection-following scrolling.

Their row contents are different and should remain backend-specific.

### Application integration

The app separately stores `selected_job_id` and `selected_process_id`, reconciles each selection during refresh, and dispatches parallel action branches in both idle and streaming key paths.

### Intentional differences

Do not abstract away these differences:

- Jobs support queued/running/done/error states, agent concurrency, activity, token counts, results, and transcripts.
- Processes support stdout/stderr, exit codes, signals, timeouts, and process cancellation.
- Agent viewing uses the transcript viewer.
- Process viewing uses the live output viewer.
- Agent cancellation is sent through `RuntimeCommand::CancelJob`.
- Process cancellation uses `ProcessRegistry::kill`.

## Target Design

### 1. Shared selectable-pane mechanics

Add a small TUI module, for example `tui/src/ui/activity_pane.rs`, containing only reusable presentation and navigation concepts.

Suggested identifiers and actions:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityId {
    Job(String),
    Process(String),
}

pub enum ActivityAction {
    Unhandled,
    SelectionChanged,
    Open(ActivityId),
    Cancel(ActivityId),
}
```

If carrying the backend kind through generic pane logic adds more code than it removes, keep backend IDs as strings and map the generic result at the call site. Choose the smallest representation after implementing the first call site.

The shared navigation function should accept:

- key code and modifiers;
- an ordered slice of active IDs;
- mutable selected ID; and
- whether Enter is allowed to open.

It should return a backend-neutral action such as selection changed, open selected, cancel selected, or unhandled.

### 2. Shared selection reconciliation

Extract a helper that ensures selection is either:

- the same stable ID if it is still active;
- the first active ID if the previous selection disappeared; or
- `None` when no active items remain.

Use this helper for both jobs and processes during pane refresh.

### 3. Shared pane layout

Extract common selected-row layout behavior without forcing jobs and processes into one row schema.

A minimal API can accept already-built `Line<'static>` rows paired with IDs, then apply:

- the `›` selection prefix;
- selected background styling;
- `visible_rows = 8`; and
- selection-following scroll calculation.

Alternatively, provide helpers for selection style and scroll only if that produces less code. Do not introduce callbacks, boxed closures, or a renderer trait unless direct helpers cannot express both panes cleanly.

Backend renderers remain responsible for row-specific content:

- Jobs: icon, agent name, activity, elapsed time, and token count.
- Processes: icon, command, and latest output.

### 4. Shared input routing

Replace `AgentsKeyResult` and `ProcessesKeyResult` with one action shape. Both idle `handle_key` and streaming `drain_keys` should invoke the same navigation helper.

Keep backend dispatch explicit:

```rust
match action {
    ActivityAction::Open(ActivityId::Job(id)) => { /* transcript viewer */ }
    ActivityAction::Open(ActivityId::Process(id)) => { /* process viewer */ }
    ActivityAction::Cancel(ActivityId::Job(id)) => { /* RuntimeCommand */ }
    ActivityAction::Cancel(ActivityId::Process(id)) => { /* registry kill */ }
    _ => {}
}
```

Do not merge the viewers or cancellation implementations.

## Implementation Phases

### Phase 1: Lock down behavior

- [ ] Review current focused pane, key-routing, and viewer tests.
- [ ] Add any missing assertions needed to preserve ordering, empty-list behavior, selection reconciliation, and modified-key fallthrough.
- [ ] Confirm tests cover both idle and streaming paths.

### Phase 2: Extract navigation and selection

- [ ] Add the shared action and navigation helper.
- [ ] Add shared stable-ID selection reconciliation.
- [ ] Migrate agent navigation without changing behavior.
- [ ] Migrate process navigation without changing behavior.
- [ ] Remove `AgentsKeyResult`, `ProcessesKeyResult`, and duplicated navigation code.

### Phase 3: Extract rendering mechanics

- [ ] Centralize the selected-row marker and background.
- [ ] Centralize visible-row and selection-following scroll calculations.
- [ ] Migrate `jobs_pane::render_selected`.
- [ ] Migrate `processes_pane::render`.
- [ ] Keep backend-specific text, icons, and status styles in their existing modules.

### Phase 4: Simplify application integration

- [ ] Use the shared reconciliation helper in pane refresh.
- [ ] Use the shared action type in idle input handling.
- [ ] Use the same action type in streaming input handling.
- [ ] Keep job/process open and cancel dispatch explicit.
- [ ] Rename `refresh_jobs_pane` if it continues to refresh both jobs and processes; use a name such as `refresh_background_panes`.

### Phase 5: Cleanup and validation

- [ ] Remove dead helpers, enums, constants, and duplicate tests.
- [ ] Check that the refactor reduces or does not materially increase TUI LOC.
- [ ] Run formatting and focused tests.
- [ ] Run the complete TUI library test suite.
- [ ] Review the final diff for behavior changes and accidental edits.

## Primary Files

Expected files:

- `tui/src/ui/activity_pane.rs` or similarly named new shared module
- `tui/src/ui/mod.rs`
- `tui/src/ui/jobs_pane.rs`
- `tui/src/ui/processes_pane.rs`
- `tui/src/ui/app/mod.rs`
- `tui/src/ui/app/stream/mod.rs`
- `tui/src/ui/app/app_tests.rs`
- `tui/src/ui/jobs_pane_tests.rs`

No core runtime file should need modification for this refactor.

## Tests

### Navigation

- [ ] Up selects the previous active item and clamps at the first item.
- [ ] Down selects the next active item and clamps at the last item.
- [ ] Missing or stale selection resolves predictably.
- [ ] Empty lists do not create an action.
- [ ] Enter opens only when input is empty.
- [ ] Enter with nonempty input remains available for message submission.
- [ ] Unmodified `k` cancels the selected item.
- [ ] Modified navigation/open/cancel keys fall through.

### Rendering

- [ ] Selected rows use the same marker and background for jobs and processes.
- [ ] Both panes retain eight visible rows.
- [ ] Selection beyond the viewport scrolls into view.
- [ ] Job-specific content remains unchanged.
- [ ] Process-specific content remains unchanged.

### Integration

- [ ] Idle agent open/cancel behavior remains unchanged.
- [ ] Idle process open/kill behavior remains unchanged.
- [ ] Streaming agent open/cancel behavior remains unchanged.
- [ ] Streaming process open/kill behavior remains unchanged.
- [ ] A completed item disappearing from a pane selects the first remaining item.
- [ ] Empty active lists remove their corresponding pane.

## Validation Commands

```text
cargo fmt --all -- --check
git diff --check
cargo test -p bone --lib
```

Run narrower focused tests during each phase where useful. Do not use the nonexistent `bone-tui` package name; the TUI package is `bone`.

## Acceptance Criteria

- Agents and Processes use one implementation for keyboard navigation and stable-ID selection behavior.
- Selection highlighting and viewport scrolling use shared mechanics.
- Idle and streaming paths consume the same backend-neutral actions.
- Agent/process row content, cancellation dispatch, registries, and viewers remain separate.
- Enter with nonempty input still submits or steers rather than opening a viewer.
- No visible pane behavior regresses.
- The refactor does not introduce a generic runtime registry or unnecessary trait hierarchy.
- Net code is neutral or smaller unless additional tests account for the increase.
- All existing uncommitted work is preserved.

## Working-Tree Safety

At plan creation time, the background-process implementation is uncommitted. Preserve these paths and any other changes reported by `git status`:

- `core/src/ext/ctx.rs`
- `core/src/processes.rs`
- `core/src/tools/shell.rs`
- `core/tests/shell_test.rs`
- `tui/src/ui/app/app_tests.rs`
- `tui/src/ui/app/mod.rs`
- `tui/src/ui/app/stream/mod.rs`
- `tui/src/ui/mod.rs`
- `tui/src/ui/processes_pane.rs`
- `tui/src/ui/process_view.rs`

Inspect relevant diffs before editing. Do not reset, replace, or reformat unrelated changes.