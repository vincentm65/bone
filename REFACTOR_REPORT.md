# Code-Quality Dedup Refactors — Final Report

**Scope:** Eliminate verbatim-duplicated code across the `bone` workspace.
**Constraints:** Rust-only (Lua untouched); net-zero-or-negative LOC per refactor.
**Method:** Verify actual LOC delta before editing; build + test + clippy per step;
lowest-risk/highest-yield first.

---

## Completed refactors (7 commits, −135 code LOC)

| # | Commit   | Target                                         | Net LOC | Files |
|---|----------|-------------------------------------------------|---------|-------|
| A | `1163e38` | Delete `apply_view_diffs` stub + merge `seed_default_lua_*` | **−48** | 3     |
| B | `f3ff03e` | Extract `report_usage` closure in `driver.rs`   | **−11** | 1     |
| C | `99f27bf` | `streaming_client()` + `http_error()` in `provider.rs` | **−15** | 3     |
| D | `0b51386` | Extract `now_secs()` into `core::util`           | **−6**  | 3     |
| E | `60aa6a7` | `save_or_revert()` helper in `config/custom.rs`  | **−3**  | 1     |
| F | `3f59786` | `picker::draw_footer()`, dedup catalog + setup   | **−29** | 3     |
| G | `bbc80aa` | Remove dead `usage_by_provider` snapshot pipeline | **−23** | 5     |
|   |          | **Total**                                        | **−135**|       |

### Details

**A. Delete `apply_view_diffs` no-op + merge `seed_default_lua_*`** (−48)
- Removed the `apply_view_diffs` stub (empty body, orphaned doc comment) and its
  call site in `tui/src/ui/app/stream/mod.rs`.
- Collapsed three near-identical `seed_default_lua_{tools,libs,commands}` functions
  into one parameterized `seed_default_lua(kind, items)` in `core/src/ext/mod.rs`.

**B. `report_usage` closure** (−11)
- The token-usage reporting tail was verbatim-duplicated (success + error paths) in
  `core/src/runtime/driver.rs`. Extracted a local `report_usage` closure.

**C. LLM provider helpers** (−15)
- `streaming_client()` (reqwest builder with SSE + bearer auth) and `http_error()`
  (status → error mapping) were duplicated between `codex.rs` and `openai_compat/mod.rs`.
  Moved to `core/src/llm/provider.rs`; both providers updated.

**D. `now_secs()` util** (−6)
- Identical `SystemTime → epoch seconds` helper duplicated in `update_check.rs` and
  `ext/catalog.rs`. Extracted to `core/src/util.rs::now_secs()`.

**E. `save_or_revert()` helper** (−3)
- The page-path-check / save / revert-on-failure tail was verbatim-duplicated in
  `set_value` and `set_provider_entry` in `core/src/config/custom.rs`. Extracted
  `save_or_revert(namespace, key, old_value)`.

**F. `picker::draw_footer()` shared footer renderer** (−29)
- The span-building `push` closure + `Paragraph` render tail (~23 lines) was
  verbatim-duplicated between `tui/src/ui/catalog.rs` and `tui/src/ui/setup.rs`.
  Moved the shared rendering to `picker::draw_footer(frame, area, &[(&str, &str)])`;
  callers now pass a slice of (key, label) pairs and keep only page-specific key sets.
  `picker.rs` was the documented home ("shared primitives for both fullscreen screens").

**G. Remove dead `usage_by_provider` snapshot pipeline** (−23)
- `SessionSnapshot.usage_by_provider` (protocol) was populated by the RPC daemon
  at 2 sites and cloned into `App.usage_by_provider` on the TUI side, but the
  TUI never reads it: `/stats` reads per-provider usage directly from the session
  DB (`usage_stats_snapshot()` / `usage_stats_range()`). The field's only consumer
  was the dead App clone.
- Removed the full dead chain: the `SessionSnapshot` field (incl. `#[serde(default)]`),
  both RPC population sites (`publish_snapshot` + conversation-load), the ctor init,
  the App field/init/clone, and the stale doc comment in `apply_snapshot`.
- `usage_by_provider_context()` and `UsageProviderContext` stay — they still feed
  `AppCtxState` (the Lua app context) at `rpc/mod.rs:385`.
- Source: dead-code scan (background worker).

---

## Evaluated and rejected

| Candidate | Reason |
|-----------|--------|
| `lua_value_to_json` (2 sites) | Intentional specialization — different branches per call site |
| `format_tokens` vs `compact_number` | Different thresholds/logic, not true duplicates |
| truncate helpers (3 sites) | Context-specialized, structurally different |
| pane-nav blocks | Structurally different despite surface similarity |
| `codex_debug_*` logging | Runtime-gated infra; dedup would obscure the gating |
| `emit_event` variants | Intentional filtering differences |
| ToolMeta HashMap merge | Too many files for the savings (~−5 across 4 files) |
| 23× `#[allow(clippy::too_many_arguments)]` | Annotations, not logic; removing risks API churn |
| `shell_quote` (2 sites) | Marginal (~−3 LOC) for added indirection |
| `pending_count()` (2 structs) | On different types (`KeyReplyRegistry` / `ApprovalReplyRegistry`); any abstraction (trait/generic) adds plumbing ≥ the 14 duplicated lines; `resolve()` differs between them |
| key-help footer | **Done** (Batch F) |
| `save_or_revert` | **Done** (Batch E) |
| `pub use ScriptOutput, ScriptRequest` re-exports | Types are used only internally in `shell.rs`; re-exports have no downstream caller, but trimming saves 0 LOC and they are a reasonable public API surface — harmless, kept |
| Dead-code scan (all other categories) | No commented-out code, no dead consts/statics, no dead enum variants, no zero-caller fns found across 74 source files (~18.8K LOC) — codebase is clean |
---

## Status

- All 7 commits on `main`, not yet pushed (7 ahead of `origin/main`).
- Working tree has pre-existing `cargo fmt` noise on ~17 untouched files
  (leftover from an earlier `cargo fmt` run); left uncommitted — not part of these
  refactors. Each refactor commit was staged file-by-file to stay isolated from it.
- Build: clean, zero warnings. Tests: pass. Clippy: 2 pre-existing warnings
  (`ptr_arg` in `jobs.rs`, `too_many_arguments` in an unrelated fn) — not introduced
  by these refactors.
