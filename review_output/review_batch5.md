# Batch 5 Review: `src/ui/` — All 24 Files

---

## /home/vincent/projects/bone/src/ui/mod.rs
- **Lines:** 13
- **Assessment:** mostly good
- **Notes:** Minimal module re-export file. Simply lists `pub mod` declarations for each submodule. No logic, no complexity. Could optionally use `#[cfg(test)]` gating on test-only modules but that is not currently relevant. No action needed.

---

## /home/vincent/projects/bone/src/ui/autocomplete.rs
- **Lines:** 124
- **Assessment:** mostly good
- **Notes:** Clean autocomplete dropdown state with filtering, scrolling, and selection. Logic is straightforward. One minor observation: `MAX_VISIBLE` is re-exported from `bottom_pane.rs` but also referenced locally — that's fine. `visible_rows()` always returns `MAX_VISIBLE as u16` regardless of whether `matches` is smaller; this could report the actual match count instead, saving the caller an extra check. Not over-engineered.

---

## /home/vincent/projects/bone/src/ui/color.rs
- **Lines:** 46
- **Assessment:** mostly good
- **Notes:** Small, focused color parsing utility. Single function with a match on named colors and a fallback to hex. Well-documented. Can be simplified by consolidating `DARKGRAY`/`DARK_GRAY`/`DARKGREY`/`DARK_GREY` variants — the `to_ascii_uppercase` on line 19 already normalizes, so matching the underscore forms is redundant after line 23's `strip_prefix('#')` normalization. Could remove duplicate aliases since `s` is already uppercased; six variants collapse to three actual values. Minor nit: the `_` branch uses `s.len() == 6` without `s.chars().all(char::is_ascii_hexdigit)` guard, so garbage like `ZZZZZZ` panics on `from_str_radix`. Overall simple enough.

---

## /home/vincent/projects/bone/src/ui/input.rs
- **Lines:** 430
- **Assessment:** can be simplified
- **Notes:** Large input state with paste-blob management, history navigation, and key dispatch. The `PasteBlob` system is clever (collapsing large pastes) but is complex for what it does. `delete_paste_backward()` iterates every paste and compares character slices by re-collecting the buffer into a `Vec<char>` on each backspace — this is O(n*m) per keystroke. Several helper methods (`cursor_word_backward`, `cursor_word_forward`, `delete_word_backward`) independently collect `Vec<char>` from `self.buffer`, wasting allocations. Could share a single char-index cache. The `apply_key` method (lines ~131-233) is a single large function handling all key combos with nested match arms — could be broken into smaller handlers per modifier group. `history_up`/`history_down` duplicate the pattern of cloning and setting cursor. `paste_mode` is set externally; making it an internal state tracked by `insert_paste` event would be cleaner. Over-engineering in the paste-blob atomic-delete logic (lines 134-155) — removing the whole token on backspace is user-friendly but adds 20 lines of fragile slice arithmetic.

---

## /home/vincent/projects/bone/src/ui/pane_page.rs
- **Lines:** 173
- **Assessment:** mostly good
- **Notes:** Clean data structure for tool-provided content pages with upsert/remove logic. `from_content` converts from an internal `PaneContent` representation to renderable `PanePage`. The conversion maps `PaneLineSpec::Plain` and `PaneLineSpec::Spans` to ratatui `Line`s. This is a reasonable adapter layer. `upsert` conflates adding vs replacing; the returned `active_page` update is correct but the dual return slightly muddies the API. Tests cover the critical `remove` edge cases well. Not over-engineered.

---

## /home/vincent/projects/bone/src/ui/prompt.rs
- **Lines:** 99
- **Assessment:** mostly good
- **Notes:** Compact blocking-prompt model with selection, scrolling, page-up/down, peek mode, and tab support. `decision()` maps `selected` index to `Accept`/`Advise`/`Cancel` — this assumes option ordering, which is brittle if options are reordered. Could use a named variant instead of positional index. `ensure_visible` logic is straightforward. `toggle_peek` works only when `full_command` is set. Overall well-contained.

---

## /home/vincent/projects/bone/src/ui/stats.rs
- **Lines:** 776
- **Assessment:** can be simplified
- **Notes:** Full-screen token usage dashboard with charts, cards, heat map, and model breakdown. This is a large standalone TUI — essentially a mini application embedded in the project. It duplicates `RawModeGuard` from `setup.rs` (same pattern, same code). The draw functions (`draw_chart`, `draw_models`, `draw_hourly_chart`, `draw_daily_activity`) are many and each does layout + style + data transformation in one pass. Could extract a layout helper and share color constants with theme.rs (currently hardcoded at the top as `const BG`, `const TEXT`, etc.). The scrollable bar chart, hourly heat map, and daily activity grid all independently compute layout. The `draw_heat_and_conversations` function (line ~668) is a thin wrapper that could be inlined. Over-engineered for what amounts to a simple read-only data viewer. The `RawModeGuard` struct is duplicated verbatim in `setup.rs` — extract to a shared location.

---

## /home/vincent/projects/bone/src/ui/setup.rs
- **Lines:** 740
- **Assessment:** can be simplified
- **Notes:** First-launch onboarding wizard — another full-screen TUI. Mirrors the architecture of `stats.rs` (same `RawModeGuard`, same `enter/leave alternate screen` pattern, same `BoneBackend` usage). The `State` machine has 5 steps (Welcome, Tools, Commands, Init, Confirm) with separate draw functions per step. The `draw_list` function at ~line 490 is the largest single draw function and handles list rendering, scrolling, checkbox toggling, and split-pane description display. Could factor list-state tracking out of `State` into a reusable list widget. The `Item` struct duplicates the `(name, desc, checked)` pattern that could be shared with other selection UIs. The `bullet()` helper is used in one place and is trivial. Helper functions `toggle`, `set_all`, `advance`, `apply` are small and clear. The step progression is simple but the rendering is verbose. `RawModeGuard` duplication with `stats.rs` is the clearest simplification target.

---

## /home/vincent/projects/bone/src/ui/subagent_pane.rs
- **Lines:** 228
- **Assessment:** mostly good
- **Notes:** Renderer for the sub-agent live pane. Single `render()` function that filters jobs by agent, groups by running/done, and builds styled lines. Helper functions (`job_label`, `icon_fg`, `job_status`, `format_tokens`) are clean and focused. `pane_agents` merges registered agents with ad-hoc job agents. `format_tokens` manually comma-separates thousands — this could use the `compact_number` function from `stats.rs` if shared. Otherwise no over-engineering. Well-structured.

---

## /home/vincent/projects/bone/src/ui/subagent_pane_tests.rs
- **Lines:** 142
- **Assessment:** mostly good
- **Notes:** Tests for `subagent_pane.rs`. Covers the `render()` return value for no agents, all idle, running agents, multi-job, and `format_tokens` edge cases (0, thousands, k, m). Good coverage. The `job()` helper is clean. The test at line 36-44 asserts on `first.contains("◑")` and `!first.contains("running")` — subtle but correct. No issues.

---

## /home/vincent/projects/bone/src/ui/theme.rs
- **Lines:** 147
- **Assessment:** mostly good
- **Notes:** Central theme struct with 13 color fields, a `Default` impl, `apply_snapshot` for Lua theme injection, and `set_highlight` for runtime overrides. Uses a `set!` macro to reduce repetition across fields. The `apply_snapshot` macro (`apply!`) reads optional fields from a snapshot struct and parses colors. `set_highlight` has a large `match` with 13 arms — this is verbose but unavoidable in Rust without reflection. Could be generated by a build script but that's overkill. The `Default` trait is implemented manually but the struct has no non-Default fields; could use `#[derive(Default)]` with custom defaults via lazy_static or a const. Minor: `set_highlight` returns `bool` but callers might not check it. Tests exist and cover the main paths. Good.

---

## /home/vincent/projects/bone/src/ui/tool_display.rs
- **Lines:** 378
- **Assessment:** over-engineered
- **Notes:** Tool row label formatting with template rendering, shell command splitting, heredoc detection, and code reflow. This is the most over-engineered file in the batch. `build_tool_row` is fine (30 lines). The complexity comes from `format_shell_command` (line ~172) and its helpers: `expand_collapsed_heredoc_line`, `find_heredoc_marker`, `read_heredoc_delimiter`, `reflow_code_payload`, `flush_code_line`. This is a mini shell-parser + code-formatter, used solely to produce a display label for shell commands in the bottom pane. The heredoc detection (lines ~200-280) walks bytes manually handles quotes, escaped chars, and delimiter rules. `reflow_code_payload` (lines ~293-341) is a character-level reindenter that tracks string literals and brace depth — used just to format the heredoc body in the label. This logic is fragile (the quote/brace tracking is a simplified parser that will break on edge cases like regexes, multi-line strings with embedded braces, etc.). Could be replaced with a much simpler truncation (e.g., only show first N chars of the command). The `subagent_dispatch_label` function (lines 87-112) is reasonable but adds further complexity. Consider removing heredoc reflow entirely and just truncating long shell commands.

---

## /home/vincent/projects/bone/src/ui/commands/mod.rs
- **Lines:** 187
- **Assessment:** mostly good
- **Notes:** Slash command dispatch and built-in definitions. `BUILTINS` is the single source of truth. `handle()` dispatches to `help`, `model_switch`, `provider_switch`, or returns `Quit`/`OpenEditor`. The `help()` function builds a styled string with ANSI escape codes (bold via `\x1b[1m`). This is a bit odd — the entire UI uses ratatui but `help()` returns raw ANSI escape sequences. This works because it's rendered via `Message::system()` which passes through to scrollback, but it's mixing abstraction layers. `model_switch` and `provider_switch` duplicate provider/model switching logic that also exists in `app/stream/mod.rs` config actions. Could potentially share but the duplication is small. No major simplification needed.

---

## /home/vincent/projects/bone/src/ui/render/mod.rs
- **Lines:** 667
- **Assessment:** can be simplified
- **Notes:** Central rendering hub — owns `Renderer` struct, terminal lifecycle, scrollback management, viewport resizing, and message flushing. The `Renderer` struct has 7 fields tracking scrollback state, streaming flush position, viewport height, etc. `flush_new_to_scrollback` (lines ~360-440) has platform-specific fast-path (`scrollback_insert_direct`) and fallback (`insert_before`). The direct path (lines ~270-350) manually constructs buffer cells, scrolls via escape codes, and redraws — 80+ lines of platform-specific terminal manipulation. This is inherently complex due to the terminal's scrollback model. `scrollback_insert_direct` is gated `#[cfg(not(windows))]` but the comment on line ~395 notes a bug where Windows was entirely excluded from scrollback flushing — the fallback was accidentally behind the same cfg gate. The `dedup_scrollback_blanks` helper (lines 63-77) and `flush_separator` are small. `StatusInfo` has 15 fields for spinner, thinking-text, and status segments — could group spinner-related fields into a sub-struct. The `logical_lines_row_count` is imported from `messages` but the function is not defined here. Overall: complex because the problem (inline TUI with scrollback) is inherently complex, but some fields could be grouped.

---

## /home/vincent/projects/bone/src/ui/render/backend.rs
- **Lines:** 203
- **Assessment:** mostly good
- **Notes:** Custom ratatui backend wrapping `CrosstermBackend`. Overrides `draw()` on non-Windows to use EL (Erase in Line) instead of spaces for background fill — this prevents trailing spaces in copied terminal output. The `background_suffix_start` and `is_background_fill` helpers detect runs of space cells with background color. The `to_crossterm_color` function maps every ratatui `Color` variant. This is a legitimate optimization for scrollback quality. On Windows it falls through to the standard backend. All the `Backend` trait methods are delegated to `inner`. No simplification needed — it's serving a specific purpose well.

---

## /home/vincent/projects/bone/src/ui/render/bottom_pane.rs
- **Lines:** 913
- **Assessment:** can be simplified
- **Notes:** The largest file in this batch — handles rendering the entire bottom pane (input field, status bar, prompt, autocomplete, tab bar, page region). The `PaneDraw` struct bundles draw arguments. `approval_pane_lines` (lines ~130-200) builds prompt/shell preview lines. `input_text` (lines ~230-270) constructs the input field display with cursor positioning. `desired_height` (lines ~300-390) computes the required viewport height accounting for prompts, input rows, autocomplete, and pages — this is a complex set of conditionals. `draw_bottom_pane_with_tick` (lines ~400-580) is the main draw function with nested conditions for prompt mode vs. input mode, tab bars, shell command preview truncation, option rendering, page tabs, and status bar. This function alone is ~180 lines. The `build_tab_bar`, `push_metric`, `shell_prompt_title`, `shell_command_preview_lines`, `prompt_option_line`, `push_prompt_text_spans`, `styled_circle_option_spans`, `cursor_split`, `push_input_text`, `rendered_input_rows`, `clamped_pane_visible_rows`, and `page_visible_rows` helpers are all reasonable individually but the file has too many responsibilities. Could split into separate modules: `input_view.rs`, `prompt_view.rs`, `pane_view.rs`, `status_view.rs`. The `PageLayout` struct is unused? It's defined but I don't see it used elsewhere. The duplicate of `COMMAND_PREVIEW_LINES` between here and where it's defined (also in this file) is fine.

---

## /home/vincent/projects/bone/src/ui/render/markdown.rs
- **Lines:** 799
- **Assessment:** over-engineered
- **Notes:** Full Markdown-to-ratatui renderer using `pulldown_cmark` for parsing and `syntect` for syntax highlighting. Supports tables (with box-drawing Unicode characters), code blocks (with syntax highlighting via an embedded Dark+ theme), blockquotes, lists (ordered and unordered), headings, inline formatting, links, strikethrough, and task lists. The `MarkdownRenderer` struct has ~20 fields tracking rendering state. The table rendering is two-pass (collect rows, compute column widths, then render). Code block rendering uses `syntect` with a 10KB+ embedded theme file (`dark_plus.tmTheme`). There are ~30 helper functions: `sy_fg`, `table_border`, `table_cell_width`, `table_total_width`, `fit_table_width`, `truncate_spans`, `table_row`, `table_pipe_row`, `aligned_cell`, `wrap_prefixed_line`, `line_width`, `words_from_spans`, `wrap_words`, etc. This is an entire markdown renderer embedded in the project. The question is whether a simpler alternative (e.g., stripping markdown, or rendering with a simpler markup language) would suffice for a TUI chat application. The complexity is disproportionate to the feature value. Could be replaced with a simpler markdown→ratatui conversion that only handles bold, italic, code, and links (the most common in LLM responses), dropping tables, syntax highlighting, and full CommonMark support.

---

## /home/vincent/projects/bone/src/ui/render/messages.rs
- **Lines:** 316
- **Assessment:** can be simplified
- **Notes:** Converts chat `Message` structs into renderable `Line`s for scrollback. Handles user messages (with background color), assistant messages (via markdown), system/tool messages, diff previews, and tool labels. The `render_diff_preview` function (lines 80-125) detects diff headers and applies removed/added background colors — the `header_spans_for_line` heuristic (lines 130-160) parses diff headers by looking for `(-<n> | +<n>)` patterns in the text. This is fragile: it assumes a specific diff format. `numbered_diff_parts` and `wrap_numbered_diff_line` parse git-style numbered diff lines. The function `pad_to_terminal_width` adds trailing spaces for full-width background fill. `truncate_to_display_width` handles CJK. `wrap_user_line` (lines 105-115) is specific to user message wrapping with "> " marker. The `render_content` dispatch is clean but `render_diff_preview` adds significant complexity for a niche use case. Could simplify by removing diff-specific rendering and just displaying diffs as plain text.

---

## /home/vincent/projects/bone/src/ui/render/wrap.rs
- **Lines:** 114
- **Assessment:** mostly good
- **Notes:** Text wrapping utilities using Unicode width. `wrap_text`, `wrap_text_with_prefix`, `wrap_plain_line`, `visual_line_count`, `take_breakable_width`, `take_width`. Small, focused, well-named. `take_breakable_width` prefers whitespace breaks over hard breaks. `wrap_plain_line` delegates to `wrap_text_with_prefix` for indented text. `visual_line_count` handles hard newlines. Could be simplified: `take_breakable_width` and `take_width` are the core — everything else is built on them. No over-engineering.

---

## /home/vincent/projects/bone/src/ui/app/mod.rs
- **Lines:** 2159
- **Assessment:** over-engineered
- **Notes:** The largest file in the entire project. The `App` struct has **38 fields** including: messages, transcript, input, streaming state, provider/model info, LLM Arc, queue, tool handler, approval mode, prompt, pending approval, cancellation, token stats, stream estimate, pages, pane visibility, thinking tail/first_shown/clear_at, session DB handle, conversation ID, sequence counter, turn timing, autocomplete, extensions, lua keymap, lua status, shown_tool_rows, subagent version, subagent refresh, quit_despite_jobs. This is too many responsibilities for a single struct. Methods are organized into files via `mod` but the struct definition and many core methods are in this one file. Key areas:
  - `new()` (~100 lines) initializes all fields, boots Lua, collects banner, applies theme/config snapshots.
  - `init_session_db`, `append_assistant_to_db`, `append_tool_result_to_db`, `record_usage_to_db` — 4 DB helper methods.
  - `apply_lua_action`, `apply_config_action`, `load_conversation` — config/action handling.
  - `rebuild_scrollback_from_transcript` — reconstructs messages from transcript.
  - `send_message`, `submit_user_turn` — the core turn submission (defined in `stream/mod.rs` but the orchestration starts here).
  - `handle_command` — slash command handler.
  - Redraw/refresh methods, key dispatch, pane management, thinking pane, etc.
  - The `estimate_context_chars` method is mentioned but not shown in the snippet.

The struct could be split: `TurnState` (streaming, cancel, timers), `SessionState` (db, conversation_id, seq), `PaneState` (pages, active_page, panes_visible, thinking*), `StreamingState` (shown_tool_rows, subagent_*). The file is 2159 lines — any file over 1000 lines should be split. The `new()` constructor initializes too many things sequentially; could use a builder pattern.

---

## /home/vincent/projects/bone/src/ui/app/editor.rs
- **Lines:** 174
- **Assessment:** mostly good
- **Notes:** External editor integration. `open_editor()` saves a temp file, shuts down the terminal, runs the editor, restores the terminal, and reads the result. `run_editor` spawns the editor asynchronously via `tokio::process`. `editor_command` parses `$VISUAL`/`$EDITOR` with a custom `split_editor_command` that handles quotes and escapes. The custom shell parser is overkill: most editors don't have spaces in their path, and `shlex::split` or `shell_words::split` would handle this more robustly. Could use the `shlex` crate instead of hand-rolling quote handling (20 lines). Tests cover the split function. Overall reasonable.

---

## /home/vincent/projects/bone/src/ui/app/keymap.rs
- **Lines:** 119
- **Assessment:** mostly good
- **Notes:** Lua keymap binding dispatch. `lookup_keymap` matches key combos against normal/insert mode bindings. `handle_keymap_action` dispatches to 4 built-in actions (toggle_panes, cycle_approval_mode, cursor_to_start, cursor_to_end). `key_matches` parses `<C-p>`, `<S-Tab>`, `<A-Left>` style key strings and matches against crossterm `KeyCode`/`KeyModifiers`. Compact and clear. The F-key parsing (`F1`-`F12`) is a bit verbose but acceptable. No over-engineering.

---

## /home/vincent/projects/bone/src/ui/app/paste.rs
- **Lines:** 119
- **Assessment:** mostly good
- **Notes:** Non-bracketed paste burst detection. `collect_non_bracketed_paste_burst` polls the event stream with a short timeout to coalesce individual `Char` events into a single paste. On Windows the timeout is 12ms, on other platforms it's 0ms (disabled). `apply_input_key_with_paste_burst` is the entry point — if a plain char is followed by more chars within the quiet window, it collects them as a paste. This is a pragmatic workaround for terminals without bracketed paste support. The logic is correct but subtle. Could use `crossterm::event::read_timeout` if available instead of manual poll loops. Minor: the `PasteKeyResult` struct wraps both action and trailing event — this is clean. No over-engineering for the problem it solves.

---

## /home/vincent/projects/bone/src/ui/app/stream/mod.rs
- **Lines:** 1236
- **Assessment:** over-engineered
- **Notes:** The streaming turn loop — the most complex piece of the UI. Contains:
  - `KeySink` struct (lines ~80-140): Tracks pending key reply slots and buffers keystrokes for blocking tools. Has 4 states (direct, runtime, owns_input, buffer). The `arm`/`deliver`/`clear_owner` protocol is carefully designed but complex.
  - `PaneOwnership` struct (lines ~150-180): Tracks which pane component IDs a blocking tool created, for cleanup on cancel.
  - `is_subagent_dispatch` helper.
  - `key_event_from_crossterm` — maps all crossterm `KeyCode` variants to the generic `pane_content::KeyEvent` (30+ match arms).
  - `tool_error` helper.
  - `send_message` (~30 lines) — dispatches `:` (inline command), `/` (slash command), or normal text.
  - `submit_user_turn` (~200 lines) — the main turn loop: creates a `Driver`, pumps runtime events, approval requests, and ticker in a `tokio::select!` loop. Handles approval prompts, edit previews, key routing, tick events, streaming finalization, DB persistence, and error reporting.
  - `pump_apply_event` — processes runtime events (TextDelta, ToolCallDelta, TokenUsage, Finished, etc.).
  - `drain_keys` — reads crossterm events and applies them to input or key sinks.
  - `drain_approval_keys` — separate key loop for approval prompts.
  - `begin_approval` / `clear_approval_pane` / `pump_show_edit_preview` — approval UI helpers.
  - `pump_tick` — updates spinner, elapsed time, subagent pane refresh.
  - Thinking pane management (`show_thinking`, `clear_thinking`, `pump_thinking`).
  - `handle_command` — slash command orchestration with Lua override checking.
  - `run_inline_command`, `show_reply`, `redraw`, `force_redraw`, `persist_runtime_config`.
  - Page management (`set_page`, `clear_page`).
  - `estimate_context_chars` — heuristic context length estimator.

This file is 1236 lines and contains the main event loop, approval handling, key management, DB writes, spinner/pane ticks, and command dispatch. Could be split into: `turn_loop.rs` (the select! loop), `key_routing.rs` (KeySink, drain_keys), `approval.rs` (approval prompts), `thinking.rs` (thinking pane), `pane_management.rs`, `db_persistence.rs`. The `KeySink` state machine is well-designed but occupies ~60 lines with buffer management. The `submit_user_turn` function is ~200 lines and is the single most critical function in the UI — it could benefit from being broken into phases (setup, loop, teardown) as separate methods.

---

## Summary

| File | Lines | Assessment |
|------|-------|------------|
| `mod.rs` | 13 | mostly good |
| `autocomplete.rs` | 124 | mostly good |
| `color.rs` | 46 | mostly good |
| `input.rs` | 430 | can be simplified |
| `pane_page.rs` | 173 | mostly good |
| `prompt.rs` | 99 | mostly good |
| `stats.rs` | 776 | can be simplified |
| `setup.rs` | 740 | can be simplified |
| `subagent_pane.rs` | 228 | mostly good |
| `subagent_pane_tests.rs` | 142 | mostly good |
| `theme.rs` | 147 | mostly good |
| `tool_display.rs` | 378 | over-engineered |
| `commands/mod.rs` | 187 | mostly good |
| `render/mod.rs` | 667 | can be simplified |
| `render/backend.rs` | 203 | mostly good |
| `render/bottom_pane.rs` | 913 | can be simplified |
| `render/markdown.rs` | 799 | over-engineered |
| `render/messages.rs` | 316 | can be simplified |
| `render/wrap.rs` | 114 | mostly good |
| `app/mod.rs` | 2159 | over-engineered |
| `app/editor.rs` | 174 | mostly good |
| `app/keymap.rs` | 119 | mostly good |
| `app/paste.rs` | 119 | mostly good |
| `app/stream/mod.rs` | 1236 | over-engineered |

**Top simplification targets:**
1. `app/mod.rs` (2159 lines) — split `App` struct into focused sub-structs
2. `app/stream/mod.rs` (1236 lines) — split turn loop into separate modules
3. `render/markdown.rs` (799 lines) — reduce scope of markdown renderer
4. `render/bottom_pane.rs` (913 lines) — split into input/prompt/pane/status views
5. `tool_display.rs` (378 lines) — remove heredoc reflow; truncate instead
6. `stats.rs` and `setup.rs` — extract shared `RawModeGuard` and color constants
7. `render/mod.rs` (667 lines) — extract spinner/status info sub-struct
