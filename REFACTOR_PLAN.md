# Refactor Plan — Reduce 21,406 LOC by 1,000+

Generated from comprehensive review of all Rust source and test files on 2026-06-07.

## Overview

| Category | Lines | Est. Savings |
|----------|------:|-------------:|
| Source files | 17,810 | ~1,080 |
| Test files | 3,596 | ~525 |
| **Total** | **21,406** | **~1,605** |

---

## Phase 1: Cross-File Deduplication (~240 LOC, high confidence, low risk)

### 1a. Extract shared shell segment splitting (command_policy ↔ tool_display) — ~55 LOC

**Files:** `src/tools/command_policy/mod.rs` (478), `src/ui/tool_display.rs` (387)

- `push_segment` — identical in both files. Move to a shared utility module.
- `shell_segments` (command_policy L273–345) and `format_shell_command` (tool_display L153–217) — nearly identical character-by-character shell-splitting. The only difference is `shell_segments` also handles `#` comments.
- Create a new `src/utils/shell_split.rs` with the shared logic. Both files import from it.

### 1b. `tests/common/mod.rs` with shared helpers — ~70 LOC

**Files:** `dynamic_tools_test.rs`, `edit_file_test.rs`, `scheduled_tasks_test.rs`, `skills_test.rs`, `tools_test.rs`, `write_file_test.rs`

- `temp_dir()` / `temp_path()` functions duplicated across 6 files. Create `tests/common/mod.rs` with one shared `temp_dir(prefix: &str)` and `temp_path(prefix: &str)`.
- `sh_quote` — duplicated in `dynamic_tools_test.rs` and `scheduled_tasks_test.rs`. Move to common.
- `fn handler_with(tool: impl Tool + 'static) -> ToolHandler` — `ToolHandler::new(ToolRegistry::new().register(...))` appears ~15 times across test files. A shared helper saves ~2 lines per occurrence.

### 1c. Merge `sync_tools`/`sync_skills` and `enabled_*` pairs (custom.rs) — ~45 LOC

**File:** `src/config/custom.rs` (359)

- `sync_tools_from_registry` (L180–207) and `sync_skills_from_registry` (L238–265) — identical logic with `"tools"/"skills"` and `tool_names`/`skill_names`.
- `enabled_tool_names` (L209–236) and `enabled_skill_names` (L267–294) — identical aside from namespace string.
- Extract parameterized helper functions taking `&str` for the namespace.

### 1d. Deduplicate `logical_line_row_count` / `wrapped_line_count` — ~10 LOC

**Files:** `src/ui/render/mod.rs` (434), `src/ui/render/messages.rs` (304)

- `logical_lines_row_count` (L317–321) and `logical_line_row_count` (L323–329) in render/mod.rs are identical to `wrapped_line_count` (L30–34) in messages.rs.
- Move to a shared utility or make one the canonical function.

### 1e. SessionWriter guard pattern macro + emit closure (agent.rs) — ~44 LOC

**File:** `src/agent.rs` (721)

- **SessionWriter macro** (~20 LOC): Every method (`append_message`, `record_real_usage`, `record_estimated_usage`, `end`) repeats the same 9-line guard pattern. A `session_op!` macro collapses each to 1-3 lines.
- **emit_event closure** (~24 LOC): 12 call sites each use 4 lines for `emit_event(request.events, &AgentEvent::...)`. A local closure `let emit = |e| emit_event(request.events, e)` halves the cost per call.

### 1f. Status bar boolean parsing macro (config/mod.rs) — ~22 LOC

**File:** `src/config/mod.rs` (293)

- Lines 164–205: 11 nearly identical blocks parsing boolean config values from custom.yaml. Each block does `.get_value(...).parse().unwrap_or(true)`. A macro or helper function condenses from ~42 lines to ~20.

---

## Phase 2: Per-File Simplification (~460 LOC, medium effort)

### 2a. `ui/stats.rs` — ViewMode dedup (~200 LOC) [860 → ~660]

#### ViewMode match repetition (largest single win: ~100 LOC)

The `ViewMode` enum's 4 variants are matched in the same order in **6 different locations**:

| Location | Lines | What |
|----------|-------|------|
| `title()`, `key()`, `prev()`, `next()` | 44–65 | 4 methods, each with 4 arms |
| `From<ViewMode> for ViewModeRef` | 71–77 | 4 arms |
| `draw_chart` | 254–258 | which bucket list to use |
| `hourly_chart_lines` | 426–430 | which HourUsage slice |
| `hourly_chart_lines` | 487–492 | title string (redundant with `title()`) |
| `range_label` | 822–826 | which bucket list |

**Fix:** Add `ALL_MODES: [ViewMode; 4]` constant. Implement `prev()`/`next()` as index-wrapping arithmetic on this array. Remove all 6 match blocks.

#### `draw_daily_activity` decomposition (~40 LOC) [lines 520–645]

This 125-line function does too much: grid geometry, data slicing, activity grid construction, stats overlays with magic row-position matching, and rendering. Split into:
- `build_activity_grid(...)` — compute cell grid
- `build_activity_stats(...)` — peak date, peak tokens, day count, totals
- `render_activity_rows(...)` — render the grid with stat overlays

The inline `match row { 0 => "peak", 1 => ..., 2 => ... }` pattern is fragile. Make stats data-driven, not position-driven.

#### `hourly_chart_lines` title dedup (~20 LOC) [lines 418–504]

A second 4-arm `match` for title strings duplicates what `ViewMode::title()` already provides. Replace with `format!("{} by hour", mode.title())`.

#### Minor items (~40 LOC total)
- `run_loop` scroll=0 repeated 6 times → `set_mode()` method (8)
- `draw` narrow/wide branching → extract `draw_narrow`/`draw_wide` (15)
- `compact_tokens` is a 1-line wrapper for `compact_number` → inline it (4)
- `heat_style` color array → replace with a `const fn` that computes colors (15)
- `build_week_axis` mutable state → iterator-based rewrite (5)
- `bucket_tokens` 2-line function used once → inline (3)
- `trunc` double `chars().count()` → cache (1)
- `draw_cards` constraints → `vec![Constraint::Percentage(16); 6]` (5)
- `activity_header` if-else chain → `[full, compact, last].iter().find()` (3)

### 2b. `ui/app/stream/mod.rs` (~172 LOC) [1233 → ~1061]

#### Repeated PaneDraw construction (~40 LOC) [5 call sites]

Every place that redraws the main UI builds the same `PaneDraw` struct with identical logic for `input`, `status_info`, `pages`, `active_page`, `pane_toggle_hint`. Extract `fn build_pane_draw(&self) -> PaneDraw`.

#### `submit_user_turn` retry unification (~20 LOC) [lines 105–265]

The retry loop (lines ~150–215) has near-identical retry logic duplicated for `wait_for_stream` failure and `consume_stream` failure. Both check `retryable && attempt < MAX_PROVIDER_ATTEMPTS` and sleep 2s. Extract the retry pattern.

#### `drain_keys` nested match flattening (~15 LOC) [lines 1140–1233]

4 levels of nesting: event loop → key press → panes-visible sub-matches → page nav matches → input apply_key match. The `max_scroll` calculation repeats for PageDown and Ctrl+Down. Extract helpers.

#### TPS refresh repeated (~15 LOC) [lines ~416, ~428, ~510]

`Self::refresh_tps` + `self.redraw_streaming_tokens` called identically after every event in the stream loop. Combine into a single helper.

#### Other items (~40 LOC total)
- `drain_keys` with 7 mutable refs → make it a `self.drain_keys()` method (10)
- Streaming state start/end → `begin_streaming_turn()` / `end_streaming_turn()` methods (10)
- `estimate_context_chars` manual char-count sums → helper macro or method (12)
- `handle_tool_calls` pane-page logic → reuse `apply_tool_live_event` (10)
- Visible pages ternary → `self.visible_pages()` helper (8)
- `StreamFailure::display_message` verbose match + `matches!` → direct match (5)
- Dead `if` branch in `wait_for_stream` elapsed calc (bug fix, both branches identical) (3)

### 2c. `ui/app/mod.rs` (~130 LOC) [2139 → ~2009]

#### `config_picker` decomposition (~30 LOC) [lines 1650–1860]

This 210-line function handles tab navigation, provider selection, field editing, config field cycling, status bar submenu detection, and tool/skill sync. Split into:
- `config_picker_tab_providers(...)` 
- `config_picker_tab_fields(...)`
- The outer navigation loop

The `providers_tab_idx` special case, field index remapping (`field_actual_idx`), and status-bar submenu detection are tangled together.

#### Tool setup duplication (~18 LOC) [~141–155 vs ~1580–1602]

The reload branch in `handle_tools_command` duplicates the tool-loading + sync + enabled-name logic from `App::new`. Extract `reload_tools(&mut self, loaded: LoadedTools)`.

#### Other items (~82 LOC total)
- Duplicated `append_assistant_to_db` / `append_tool_result_to_db` → merge into one (15)
- Repeated `flush_new_to_scrollback` + `redraw` at ~12 sites → `flush_and_redraw()` helper (10)
- Duplicated queue processing logic → `process_queue()` method (8)
- Pane scroll duplication in `handle_key` → `scroll_page(delta)` helper with shared `max_scroll` (15)
- `handle_command` cascading `if` chain → `match` with grouped arms (5)
- `provider_editor` field repetition → data-driven `Vec<(&str, &str, bool)>` descriptors (15)
- `handle_skills_command` inline reply building → extract helper (5)
- Verbose `match` in `compact_transcript_state` → single expression (4)
- `prompt_and_wait` advising sub-loop → extract `prompt_and_wait_advising()` (5)

### 2d. `ui/render/bottom_pane.rs` (~100 LOC) [857 → ~757]

#### render_line helper (~40 LOC) [12+ call sites]

This 4-line pattern appears constantly:
```rust
frame.render_widget(
    Paragraph::new(Span::styled(content, style)),
    Rect { y, height: 1, ..area },
);
y += 1;
```
Extract `fn render_line(frame, area, y: &mut u16, content, style)` with bounds checking. Also standardizes inconsistent bounds checks across sites.

#### Duplicated tab bar rendering (~25 LOC) [lines 325–356 and 647–682]

Both the prompt tab bar and page tab bar iterate over items, add `" | "` separators, bold the active item, and render as a `Line`. Extract `fn render_tab_bar(tabs, active_idx, ...)` with call-site customization for the "Tab to switch" hint suffix.

#### Token metric builder repetition (~25 LOC) [lines 706–770]

4 nearly identical blocks building status bar token metrics. Each checks a config flag, pushes a separator if not first, then pushes the metric. Helper:
```rust
fn push_token_metric(parts: &mut Vec<Span>, show: bool, label: &str, value: String, ...)
```

#### Minor items (~10 LOC)
- `prompt_option_line` verbose style variables → inline the expressions (10)
- Duplicated right-padding pattern for prompt hint and autocomplete → share (8)
- `push_input_text` only called from `input_text` → inline it (5)

---

## Phase 3: Smaller Source File Refactors (~380 LOC, low effort)

### `tools/command_policy/mod.rs` (~35 LOC excl. shared extraction)

- `contains_static_name` and `contains_config_name` → unify with generic `fn contains_name<T: AsRef<str>>` (5)
- `matches_ignore_ascii_case` → inline at 2 call sites (6)
- `CommandSafety::rank()` only used by `max()` → inline (4)
- `normalize_list` and `normalize_shell_wrappers` → parameterize (5)
- `classify_segment` subcommand checks → extract `fn check_subcommand(names, cmd, subs)` (10)
- Dead `mode` parameter in `prepare_tool_calls` → remove (3)

### `session_db.rs` (~67 LOC)

- SQL string duplication in `usage_by_model_since` and `usage_by_hour_since` → inject WHERE clause with `Option::map_or` (18)
- Repetitive filter calls in `usage_stats_snapshot` → `date_filter(ViewModeRef)` helper (15)
- `UsageSummary` manual zero-init → `#[derive(Default)]` + `UsageSummary::default()` (5)
- `searchable` construction → `Option::map_or` (2)
- Schema SQL as inline string → `include_str!("../sql/schema.sql")` (50 moved, ~17 saved in function)
- Macro for prepare+query_map pattern (~27)

### `openai_compat/mod.rs` (~32 LOC)

- `flush_partial_tool_calls` 3× boilerplate → `PartialToolCallManager` struct with `drain_events()` (12)
- `StreamOptions` struct → `serde_json::json!({ "include_usage": true })` inline (5)
- `PartialToolCall` BTreeMap management → struct with `.process_delta()` and `.flush()` (15)

### `main.rs` (~36 LOC)

- Subcommand matching repeated 4× → `fn is_subcommand(args, name) -> bool` (6)
- `map_err(std::io::Error::other)` repeated 7+× → `fn io_err(e) -> std::io::Error` (8)
- `try_install` package manager if-chain → data-driven `const PACKAGE_MANAGERS: &[&[&str]]` (18)
- `ensure_deps` marker path → `bone::config::bone_dir()` reuse (4)

### `markdown.rs` (~38 LOC)

- SoftBreak/HardBreak code branch duplication → merge `Event::SoftBreak | Event::HardBreak` (5)
- Repeated `finish_line() + blank_line()` pairs → `finish_and_blank()` helper (4)
- Verbose style modifier repetition in `current_style` and `syntect_style` → helper method (10)
- `unwrap_markdown_table_fences` manual index loop → iterator-based with `split_inclusive` (8)
- `render_markdown` event loop → `handle_start_tag` / `handle_end_tag` methods (structural, ~0 LOC)
- `list_stack` double-indirection → `enum ListItemType` (3)
- Inline closure for quote-only check → helper method (2)
- Redundant bold toggle in heading → remove, heading already implies bold in `current_style` (2)

### `dynamic.rs` (~30 LOC)

- Exit code error formatting 3× → `fn check_exit_code(output) -> Result` (8)
- Output kind dispatch 2× → `fn parse_output(&self, stdout) -> Result` on DynamicTool (5)
- Dead `Some(OutputKind::JsonlEvents)` match arms → remove from both matches (2)
- `parse_jsonl_events` if-else chain → `match event["type"].as_str()` (7)
- `LiveStateGuard::drop` double early return → flatten to `if let` (3)
- `PaneLineDef::into_line` unnecessary `.collect::<Vec<_>>()` → remove (1)
- `parse_color` 12-arm match → const lookup table (2)
- `From<PaneEnvelope>` impl for PanePage → removes manual field copy (3)

### `edit_file/mod.rs` (~14 LOC)

- Dead exclusivity check in `parse_operation` — the `kinds` counter already enforces exactly one operation kind (6)
- Dead `match_mode` check — JSON schema already constrains `match_mode` (2)
- Impossible dedup in `line_window_candidates` — loop invariant guarantees uniqueness (3)
- `ensure_no_edit_fields_for_rewrite` single-use function → inline (3)

### `codex.rs` (~20 LOC)

- `extract_output_index()` extracted 4× → helper function (9)
- `response.completed` verbose dedup → `if !emitted_tool_call_ids.insert(id) { continue; }` (8)
- `tools.is_empty()` → `None` boilerplate → `(!codex_tools.is_empty()).then_some(codex_tools)` (3)

---

## Phase 4: Test File Consolidation (~455 LOC, easy)

### `tests/dynamic_tools_test.rs` (~140 LOC) [730 → ~590]

- Inline YAML tool definitions have ~15 identical fields. Create a `make_tool!(name, output_kind, script_body)` macro. Used ~12 times (80)
- `json_envelope_pane_visible_rows_is_preserved` and `json_envelope_pane_scroll_is_preserved` differ only in one field → parameterized test (20)
- `line_envelope_empty_pane_lines` and `json_envelope_empty_pane_lines` test same concept → parameterize (30)
- `temp_dir` → shared (6)
- `sh_quote` → shared (3)

### `tests/integration_test.rs` (~120 LOC) [278 → ~158]

- `RecordingTool` duplicates `MockTool` from `stream_tools_test.rs` → use the mock or a shared fixture (50)
- `disabled_tools_are_not_advertised_or_executed` duplicates `tool_handler_execute_all_disabled_tool` in stream_tools_test.rs (20)
- Custom config tests have ~15 lines of `CustomConfigPage` setup boilerplate each × 4 → builder macro (50)

### `tests/openai_compat_test.rs` (~60 LOC) [339 → ~279]

- `test_done_flushes_partial_tool_calls` and `test_stream_ends_without_done_still_flushes` → parameterize (12)
- `test_done_emits_token_usage` 20 lines for two assertions → condense (15)
- `test_text_then_usage` duplicates logic from `test_usage_chunk_updates_last_usage` → merge (12)
- `test_single_tool_call_split_across_chunks` and `test_multiple_tool_calls_interleaved` → parameterize by index count (20)

### `tests/edit_file_test.rs` (~55 LOC) [499 → ~444]

- `preserves_file_contents_on_failed_duplicate_search` and `preserves_file_contents_on_missing_search` overlap with `refuses_missing_search_string` and `refuses_duplicate_search_string_with_line_numbers` (25)
- `multi_edit_failure_is_atomic` × 2 (basic and post-recovery) → combine (12)
- `search_replace_uses_replace_when_text_is_also_present` and `search_replace_accepts_text_as_replace_fallback` → parameterize (10)
- `temp_path` → shared (7)

### `tests/command_policy_test.rs` (~50 LOC) [654 → ~604]

- `for_call_ignores_model_classification`, `policy_overrides_model_classification`, `shell_policy_is_source_of_truth` → merge into one parameterized test (30)
- `compound_newlines_comments_and_pipes_readonly` uses fragile real paths → simplify (20)

### `tests/bottom_pane_test.rs` (~45 LOC) [444 → ~399]

- `bottom_separator_can_show_pane_toggle_hint` and `bottom_separator_hint_uses_display_width` → parameterize (20)
- `single_pane_page_has_only_the_fixed_bottom_separator` and `pane_page_renders_content_between_input_and_status` → merge (25)

### `tests/stream_tools_test.rs` (~35 LOC) [310 → ~275]

- `MockTool` and `SlowTool` overlap → single parameterized mock with optional delay (20)
- `stream_failure_retryable_cases` and `stream_failure_non_retryable_cases` → parameterize (15)

### `tests/render_test.rs` (~20 LOC) [342 → ~322]

- 6 block quote tests covering similar ground → consolidate to 3-4 (20)

---

## Priority Order (by impact / risk ratio)

| # | Item | LOC | Risk | Phase |
|---|------|----:|------|-------|
| 1 | ViewMode match dedup (stats.rs) | 100 | Low | 2a |
| 2 | Shell segment extraction (shared module) | 55 | Low | 1a |
| 3 | sync_tools/sync_skills merge (custom.rs) | 45 | Low | 1c |
| 4 | PaneDraw builder (stream/mod.rs) | 40 | Low | 2b |
| 5 | SessionWriter macro + emit closure (agent.rs) | 44 | Low | 1e |
| 6 | render_line helper (bottom_pane.rs) | 40 | Low | 2d |
| 7 | Status bar bool parsing (config/mod.rs) | 22 | Low | 1f |
| 8 | test shared helpers (tests/common) | 70 | Zero | 1b |
| 9 | draw_daily_activity split (stats.rs) | 40 | Medium | 2a |
| 10 | submit_user_turn retry (stream/mod.rs) | 20 | Medium | 2b |
| 11 | config_picker split (app/mod.rs) | 30 | Medium | 2c |
| 12 | try_install data-driven (main.rs) | 18 | Low | 3 |
| 13 | dynamic_tools_test make_tool! macro | 80 | Low | 4 |
| 14 | integration_test.rs dedup | 120 | Low | 4 |
| 15 | openai_compat PartialToolCallManager | 32 | Low | 3 |
| 16 | session_db SQL dedup | 67 | Low | 3 |
| 17 | markdown.rs items | 38 | Low | 3 |
| 18 | codex.rs items | 20 | Low | 3 |
| 19 | edit_file/mod.rs dead code | 14 | Low | 3 |
| 20 | dynamic.rs helpers | 30 | Low | 3 |
| 21 | remaining test consolidations | 230 | Low | 4 |

---

## Risk Assessment

**Zero risk (pure extract helpers, no behavior change):**
- render_line helper, PaneDraw builder, SessionWriter macro, test common helpers, all macro-based consolidations

**Low risk (remove dead code, simplify checks):**
- ViewMode dedup, dead match arms, dead validation checks, schema SQL extraction, repetitive filter calls

**Medium risk (restructure function, split logic):**
- config_picker split (easy to introduce a regression in tab navigation)
- draw_daily_activity split (cell grid reconstruction)
- submit_user_turn retry unification (the two retry paths may have subtle behavioral differences)

**Bug fix (not a refactor):**
- Dead `if` branch in `wait_for_stream` elapsed calculation (stream/mod.rs line 1089–1097) — both branches of the `if` execute the identical line. This is either dead code or a missing case for "currently paused."

---

## Implementation Note

After each change:
1. `cargo check` for compile-time correctness
2. `cargo test` for regression
3. `cargo clippy` for dead code/cleanup

Start with Phase 1 items. They are mechanical extractions with near-zero risk and give quick line savings. Phase 2 items save the most lines but require more judgment during implementation. Phase 3-4 are simple, isolated changes that can be done in any order.
