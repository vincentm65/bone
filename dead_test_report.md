# Dead Test Code Report

## Summary

**6 dead files** containing **~103 tests total**. These tests fall into two categories:
- **Integration tests** in `tests/` (3 files) — compiled as separate binaries but fail to compile due to visibility/import issues
- **Orphan source files** in `src/ext/` (3 files) — not declared as modules, never compiled

All test content is **duplicated** from inline `#[cfg(test)] mod tests` blocks already present in the corresponding source files (`ctx.rs`, `jobs.rs`, `loader.rs`). No unique test coverage is lost by removing them.

---

## Category 1: Integration Tests (`tests/`) — Compile Failures

### `tests/ctx_tests.rs` (20 tests)
**Reason: cannot access `pub(crate)` items from external tests**

References these non-public items:
- `bone::ext::ctx::parse_agent_opts` — `pub(crate)`
- `bone::ext::ctx::opt_get` — `pub(crate)`
- `bone::ext::ctx::tool_call_result` — `pub(crate)`
- `bone::ext::ctx::make_session_current` — `pub(crate)`
- `bone::ext::ctx::spawn_err` — `pub(crate)`
- `bone::ext::ctx::agent_err_table` — `pub(crate)`
- `bone::ext::ctx::agent_depth_exceeded` — `pub(crate)`
- `bone::ext::ctx::UsageContext` — `pub(crate)`
- `bone::ext::ctx::UsageProviderContext` — `pub(crate)`
- `bone::tools::ApprovalMode` — imported incorrectly as `crate::tools::ApprovalMode`

These tests already exist inline in `src/ext/ctx.rs` (within `#[cfg(test)] mod tests { use super::*; }`).

### `tests/jobs_tests.rs` (26 tests)
**Reason: `MAX_RETAINED_JOBS` is `pub(crate)`, not accessible from external tests**

Fails on line 130:
```rust
for i in 0..(MAX_RETAINED_JOBS + 10) {
```

Also fails on lines referencing `finish` with `FinishOptions` — `FinishOptions` field is `pub(crate)`.

These tests already exist inline in `src/ext/jobs.rs`.

### `tests/loader_tests.rs` (5 tests)
**Reason: `bone::ext::loader` module is private**

```rust
use bone::ext::loader::{with_bone, collect_snapshot};
// error: module `loader` is private
```

These tests already exist inline in `src/ext/loader.rs`.

---

## Category 2: Orphan Source Files (`src/ext/`) — Never Compiled

### `src/ext/ctx_tests.rs` (20 tests)
- Not declared as `mod ctx_tests` anywhere in `src/ext/mod.rs` or `src/lib.rs`
- Uses `use super::*;` — identical content to the inline `#[cfg(test)]` block in `ctx.rs`
- Death confirmed: `grep -rn 'mod ctx_tests' src/` returns no results

### `src/ext/jobs_tests.rs` (27 tests)
- Not declared as `mod jobs_tests` anywhere
- Uses `use super::*;` — identical to inline `#[cfg(test)]` block in `jobs.rs`
- Death confirmed: `grep -rn 'mod jobs_tests' src/` returns no results

### `src/ext/loader_tests.rs` (5 tests)
- Not declared as `mod loader_tests` anywhere
- Uses `use super::*;` — identical to inline `#[cfg(test)]` block in `loader.rs`
- Death confirmed: `grep -rn 'mod loader_tests' src/` returns no results

---

## Active / Working Tests (not dead)

These 6 test files compile and pass successfully:
- `tests/approval_test.rs` — 2 tests
- `tests/compact_test.rs` — 12 tests
- `tests/lua_api_test.rs` — 8 tests
- `tests/lua_tool_nested_test.rs` — 1 test
- `tests/subagent_test.rs` — 7 tests
- `tests/integration_test.rs` — 13 tests

Plus inline unit tests in `src/ext/ctx.rs` (20 tests), `src/ext/jobs.rs` (27 tests), `src/ext/loader.rs` (5 tests), and other source files compiled as `--lib`.

---

## Recommended Action

**Delete all 6 files.** No code changes needed — all tests already exist as inline `#[cfg(test)]` blocks in their respective source files, using `use super::*;` which gives full access to private/crate-internal items.

Files to delete:
1. `tests/ctx_tests.rs`
2. `tests/jobs_tests.rs`
3. `tests/loader_tests.rs`
4. `src/ext/ctx_tests.rs`
5. `src/ext/jobs_tests.rs`
6. `src/ext/loader_tests.rs`
