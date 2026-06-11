# Remaining Subagent Work

This file is the handoff for the interrupted subagent architecture work. The implementation changes are in the tree, but test cleanup, documentation, and runtime verification are still pending.

## Verified Current State

`cargo check --all-targets` currently fails with 6 errors, all in `tests/subagent_test.rs`.

The library and other targets compile far enough to reach only this test file.

## Remaining Required Work

### 1. Fix `tests/subagent_test.rs` compile errors

Exact verified errors from `cargo check --all-targets`:

- `tests/subagent_test.rs:159`
  - `JobRegistry::take_finished_unconsumed()` no longer exists.
  - Replace with `peek_finished_unconsumed()` plus explicit `mark_consumed(...)` where the test needs consume-once behavior.

- `tests/subagent_test.rs:171`
  - Same removed `take_finished_unconsumed()` call.
  - Use `peek_finished_unconsumed()` plus `mark_consumed(...)`.

- `tests/subagent_test.rs:327`
  - Same removed `take_finished_unconsumed()` call.
  - Use `peek_finished_unconsumed()` plus `mark_consumed(...)`.

- `tests/subagent_test.rs:429`
  - `registry.complete(&id1, ...)` now receives `id1: Result<String, String>` because `registry.create(...)` returns `Result`.
  - Unwrap or otherwise handle the create result before calling `complete`.

- `tests/subagent_test.rs:430`
  - Same issue for `id2`.
  - Unwrap or otherwise handle the create result before calling `complete`.

- `tests/subagent_test.rs:435`
  - `ExtensionManager::render_subagent_pane(...)` was removed.
  - Rewrite this test against `bone::ui::subagent_pane::render(...)`, or delete it if the unit tests in `src/ui/subagent_pane.rs` cover the behavior sufficiently.

Additional test notes:

- Use unique agent names per test where possible. The job registry is process-global.
- Spawn-related tests now need to account for atomic busy rejection from `JobRegistry::create(...)`.
- Any consume-once assertions should check the new peek/mark contract explicitly.

### 2. Run full verification

After fixing the test compile errors, run:

```sh
cargo test
cargo clippy --all-targets
```

No full test run was completed after the current subagent changes.

### 3. Update `defaults/AGENTS.md`

Document the new subagent behavior and extension API details:

- `bone.agent_depth`
- `bone.headless`
- `timeout_ms` in `register_subagent`
- Inactivity-based timeout semantics: timeout is based on no received agent activity, not a hard wall-clock cutoff while work is streaming.
- No nested subagents: subagent tool registration is skipped when `bone.agent_depth > 0`, and Rust also enforces max depth.
- Result spill files under `temp_dir()/bone-jobs/job-N.txt` and the `result_file` field.
- Rust-rendered subagent pane.
- Removal of the `bone._subagents_render` Lua pane hook.
- Headless behavior: dispatch waits because there is no TUI auto-injection path.
- Quit guard: first quit warns while jobs are running; second quit exits anyway.

### 4. Optional read-only wording polish

Mechanism is already correct: read-only subagents skip disallowed commands and the agent loop continues.

Optional change in `src/agent.rs::execute_tool_calls` around the denial message:

Current message shape:

```text
[exit_code=1] Tool not executed. Approval mode {mode_label} does not allow {safety:?}.
```

Consider making this clearer for read-only subagents so they continue with read-only tools instead of retrying the denied command.

### 5. Seeded Lua migration decision

Verify the behavior of `seed_default_lua_tools`.

If it never overwrites existing user files, users with an existing `~/.bone-rust/lua/tools/subagent.lua` will keep the old Lua implementation. That old file may still include:

- `bone._subagents_render`
- `pane` returns
- `cjson.encode({ content = ..., pane = ... })`

The old render hook is harmless if Rust never calls it, but old JSON-shaped return strings may show up poorly. Decide whether to add a migration note, version bump, explicit warning, or overwrite strategy.

### 6. Runtime verification checklist

Verify these manually in the TUI and CLI:

- TUI subagent dispatch starts a job and the pane appears.
- Pane updates while running, approximately once per second.
- Pane does not freeze while the main Lua VM is blocked elsewhere.
- Finished subagent results are auto-injected exactly once.
- User draft text is preserved when results are injected.
- Quit with running subagents warns first, then exits on a second quit.
- Headless CLI dispatch waits for the subagent result.
- Read-only subagent skips disallowed commands and continues with allowed read-only tools.

## Completed This Round

- Added `BootOptions` with `agent_depth` and `headless`.
- Plumbed boot options through extension initialization and agent/headless paths.
- Exposed `bone.agent_depth` and `bone.headless` to Lua.
- Enforced no nested subagents in Lua registration and Rust depth handling.
- Reworked subagent Lua tool behavior, including headless force-wait and `timeout_ms` forwarding.
- Added inactivity-based timeout plumbing so active agents are not killed by a hard wall-clock cutoff.
- Replaced Lua-rendered subagent pane with Rust-native pane rendering.
- Added job registry peek/mark-consumed flow and job snapshot access.
- Added result spill-file references to formatted subagent results.
- Added quit guard for running subagent jobs.
- Added subagent system prompt composition for depth > 0.
- Added `src/ui/subagent_pane.rs` unit coverage for the new pane renderer.
