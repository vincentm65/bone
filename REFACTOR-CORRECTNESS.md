# Correctness Track — Fix First

Genuine bugs with user-visible impact. Each is small, self-contained, and does
**not** depend on the larger maintainability refactor. Ship as independent commits.

> Line numbers from the original audit are stale — re-locate each site before editing.

---

## C1. Destructive schema migrations (was audit #4)
**File:** `src/session_db.rs` (~line 230, `init`/migration block)
**Bug:** On any `user_version != SCHEMA_VERSION`, the code `DROP`s `messages_fts`,
`messages`, `usage_events`, `conversations` and recreates from scratch. Every
schema bump silently destroys all user conversation history and usage stats.
**Fix:** Replace drop-and-recreate with a versioned migration chain. Keep the
full `CREATE` only for the fresh-DB (`user_version == 0`) path; for each version
bump apply data-preserving `ALTER TABLE` steps, then set `user_version`.
**Risk if untouched:** Permanent data loss for every existing user on next bump.

## C2. SQL built with `format!()` on raw filters (was audit #12)
**File:** `src/session_db.rs` — `usage_by_model_since` / `usage_by_hour_since`
**Bug:** SQL assembled via `format!()` interpolating a raw `date_filter` string
(injection-by-convention; fragile even if currently caller-controlled).
**Fix:** Use bound parameters (`?`) instead of string interpolation. No behavior
change; eliminates the injection surface and the stringly-typed date handling.
**Scope note:** Just the parameterization. The "merge 14 queries into 3" and
CTE-dedup cleanup from audit #12 is *maintainability*, not correctness — defer it.

## C3. `panic!` crashes the Lua VM on I/O error (was audit #21)
**File:** `src/ext/ops_plugins.rs` — `list` uses `unwrap_or_else(|e| panic!(...))`
**Bug:** A plugin-directory I/O error panics inside a Lua closure, taking down
the whole VM instead of surfacing a recoverable Lua error.
**Fix:** Propagate the error as a Lua error (return `Err`/error table) so the
script can handle it. Leave the broader `plugins/` submodule restructure for the
maintainability track.

---

## Order
1. **C1** — only one with data-loss impact; do it first.
2. **C2** — quick, isolated.
3. **C3** — quick, isolated.

## Before starting
Check existing test coverage for `session_db` migrations. If there are no tests
that exercise an upgrade path, add a characterization test (open old-version DB →
migrate → assert rows survive) as part of C1 — it's the safety net for the whole
correctness track.
