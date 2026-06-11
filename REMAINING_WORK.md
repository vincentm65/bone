# Subagent Work Status

The remaining compile, test, documentation, and wording tasks from the previous handoff have been implemented.

## Completed

- Fixed all 6 `tests/subagent_test.rs` compile errors.
- Updated tests from `take_finished_unconsumed()` to the `peek_finished_unconsumed()` / `mark_consumed(...)` contract.
- Updated registry create-call tests to handle `Result<String, String>`.
- Rewrote the stale render-path test to call `bone::ui::subagent_pane::render(...)` directly.
- Documented `bone.agent_depth`, `bone.headless`, `timeout_ms`, inactivity timeout behavior, no-nesting behavior, result spill files, Rust-rendered pane behavior, headless force-wait behavior, quit guard behavior, and seeded Lua migration caveat in `defaults/AGENTS.md`.
- Polished the read-only denial message so skipped disallowed calls tell the agent to continue with allowed read-only tools or report the limitation.
- Verified `seed_default_lua_tools` does not overwrite existing user Lua tools; the migration caveat is documented.
- Updated stale internal registry documentation that still referenced the removed consume API.

## Verification Run

These commands now pass:

```sh
cargo check --all-targets
cargo test
cargo clippy --all-targets
```

`cargo clippy --all-targets` exits successfully, but the project still emits existing warnings unrelated to this final cleanup pass.

## Manual Runtime Verification Still Recommended

These require a real TUI or CLI run with provider credentials/configuration:

- TUI subagent dispatch starts a job and the pane appears.
- Pane updates while running, approximately once per second.
- Pane does not freeze while Lua is blocked in a wait.
- Finished subagent results are auto-injected exactly once.
- User draft text is preserved when results are injected.
- Quit with running subagents warns first, then exits on a second quit.
- Headless CLI dispatch waits for the subagent result.
- Read-only subagent skips disallowed commands and continues with allowed read-only tools.
