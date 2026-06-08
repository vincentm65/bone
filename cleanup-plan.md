# Refactoring Plan — Remove ~200 Lines Without Losing Functionality

Each item is independent. Tackle in any order. Compile after each.

---

## 1. Merge `ViewMode` / `ViewModeRef` into one enum (~40 lines)

**Problem:** Two identical enums in different files, plus a `From` impl and 9 repeated 4-way matches.

- `stats.rs:51` defines `ViewMode` (Today, SevenDays, FourWeeks, Months)
- `session_db.rs:90` defines `ViewModeRef` — same 4 variants
- `stats.rs:94-101` has a `From<ViewMode> for ViewModeRef` impl
- 9 sites do the same `match mode { Today => ..., SevenDays => ..., ... }` to pick buckets

**Fix:**
- Delete `ViewModeRef`. Rename `ViewMode` and move it to `session_db.rs` (or a small shared module).
- Add methods on `UsageStatsSnapshot`: `buckets(mode)`, `hourly(mode)`, `models(mode)` — each returns the right slice. This replaces the repeated matches in both `session_db.rs` and `stats.rs`.
- Re-export `ViewMode` from `stats.rs` for the UI code.

**Files:** `session_db.rs`, `ui/stats.rs`

---

## 2. Extract SQL query helper in `session_db.rs` (~30 lines)

**Problem:** 4 functions (`usage_today_by_hour`, `usage_recent_days`, `usage_recent_weeks`, `usage_buckets`) each contain:
- A recursive CTE generating time labels (~15 lines)
- An identical `UsageBucket` row-mapping closure (~7 lines)
- Identical error handling (~3 lines)

Only the time dimension (hour/day/week/month) differs.

**Fix:** Extract a helper:
```rust
fn query_buckets(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<Vec<UsageBucket>>
```
The row mapping becomes one function instead of 4 copies. Each public method becomes a one-liner call.

**File:** `session_db.rs`

---

## 3. Collapse `status_show_*` fields in `config/mod.rs` (~30 lines)

**Problem:** 10 identical `bool` fields, 10 identical `Default` assignments, 10 identical `apply_custom_configs` lines — all doing the same thing.

```rust
// 30 lines that could be ~5
pub status_show_model: bool,
pub status_show_approval: bool,
// ... 8 more
```

**Fix:** Use a `HashMap<String, bool>` or a small macro for the fields. Simplest approach — a macro:
```rust
macro_rules! status_fields {
    ($($name:ident),* $(,)?) => {
        $(pub $name: bool,)*
        fn apply_status_toggles(&mut self, custom: &CustomConfig) {
            $(self.$name = bool_config(custom, stringify!($name));)*
        }
    }
}
```
Keeps the public API identical but removes the boilerplate.

**File:** `config/mod.rs`

---

## 4. Remove 3 duplicate serde deserializers in `providers_config.rs` (~25 lines)

**Problem:** Three near-identical functions: `string_or_default`, `string_or_default_endpoint`, `string_or_default_handler`. All deserialize `String` with a fallback. The `#[serde(default = "...")]` attribute already handles the missing-field case.

**Fix:** Replace all three with a single generic function or just use `#[serde(default = "fn_name")]` directly without custom deserializers. The custom deserializers only catch the case where the field is present but empty — handle that with a post-deserialize validation or a single `deserialize_non_empty_string` function.

**File:** `config/providers_config.rs`

---

## 5. Extract shared provider SSE stream loop (~40 lines)

**Problem:** `chat_stream` in `codex.rs` (580 lines) and `openai_compat/mod.rs` (375 lines) share:
- HTTP error handling (~5 lines each)
- SSE stream setup + `try_stream!` block (~10 lines each)
- Event loop boilerplate: skip comments, check `[DONE]`, yield events (~15 lines each)
- `flush_partial_tool_calls` on stream end (~5 lines each)

Plus `flush_partial_tool_calls` is implemented twice (once per file, ~20 lines each) with identical logic but different types.

**Fix:**
- Unify `PartialToolCall` and `PartialCodexToolCall` into a single struct (just `id`, `name`, `arguments` — the field name difference is trivial).
- Extract one `flush_partial_tool_calls` that works on the unified type.
- Extract an `sse_chat_stream` helper that takes the response and a chunk-parser closure, handling the SSE loop boilerplate.

**Files:** `llm/providers/openai_compat/mod.rs`, `llm/providers/codex.rs`

---

## 6. Merge `record_real_usage` / `record_estimated_usage` in `agent.rs` (~5 lines)

**Problem:** Two identical methods that differ only in a `bool` parameter (`is_estimated`).

**Fix:** One method with a `bool` parameter. Update 2 call sites.

**File:** `agent.rs`

---

## 7. Extract `find_page` helpers in `config/custom.rs` (~15 lines)

**Problem:** `self.pages.iter().find(|(ns, _)| ns == namespace)` appears 4 times. `self.pages.iter().position(...)` appears 3 times.

**Fix:** Add `page_ref(&self, ns)`, `page_mut(&mut self, ns)`, `page_index(&self, ns)` helpers. Replace all 7 sites.

**File:** `config/custom.rs`

---

## 8. Remove dead code (~5 lines)

- `ApprovalMode::allows_call` in `tools/mod.rs:161` — never called (callers go through `ToolHandler::allows_call` instead).
- `compact_tokens` wrapper in `stats.rs:740` — just delegates to `compact_number`. Replace 8 call sites.

**Files:** `tools/mod.rs`, `ui/stats.rs`

---

## Summary

| # | Target | Lines Saved |
|---|--------|-------------|
| 1 | Merge ViewMode enums + bucket accessors | ~40 |
| 2 | SQL query helper | ~30 |
| 3 | status_show_* HashMap | ~30 |
| 4 | Serde deserializer dedup | ~25 |
| 5 | Unified PartialToolCall + flush | ~40 |
| 6 | record_*_usage merge | ~5 |
| 7 | find_page helpers | ~15 |
| 8 | Dead code (compact_tokens) | ~5 |
|   | **Total** | **~190** |

## Results

**Before:** 7366 lines (src/)  
**After:** 7325 lines (src/) — **41 lines removed** (net, after adding helpers/unifying code)

The raw removal was ~220 lines; ~180 lines of new helper code were added, yielding the net reduction.
All 8 items completed. Compiles clean. All tests pass (1 pre-existing failure unrelated to changes).
