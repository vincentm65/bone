# Plan: Shrink `src/ui/app/mod.rs` (2425 lines)

No dead code. Build + clippy are clean. Goal is to extract cohesive chunks
into submodules to bring the file down to the core state machine.

## Baseline
- `src/ui/app/mod.rs` = 2425 lines
- `src/ui/app/stream/mod.rs` (already extracted) = 1241 lines
- `cargo build` + `cargo clippy`: zero dead_code/unused warnings

## Steps (in order, lowest-risk first)

### 1. Editor helpers → `ui/app/editor.rs`
- `run_editor`, `editor_command`, `default_editor`, `split_editor_command`,
  `shell_quote` (lines 2138–2318, ~180 lines)
- Pure free functions, no `App` state. Trivial extraction.
- Move `#[cfg(test)]` editor-split tests with them.

### 2. Paste-burst helpers → `ui/app/paste.rs`
- `PasteKeyResult`, `PasteBurst`, `non_bracketed_paste_quiet_timeout`,
  `plain_char`, `is_paste_burst`, `collect_non_bracketed_paste_burst`,
  `apply_input_key_with_paste_burst` (lines 28–135, ~100 lines)
- All free functions + two small structs. No `App` borrow conflicts.

### 3. `apply_lua_config_snapshot` + `key_matches` → stay or move with keymap
- `apply_lua_config_snapshot` (2385, free fn)
- `key_matches` (2325, free fn used only by `lookup_keymap`)
- Move both to `ui/app/editor.rs`? No — bundle into a `keymap.rs` if step 5
  happens, else leave.

### 4. Menu/picker UI → `ui/app/pickers.rs`
- `panel_key`, `close_panel`, `show_reply`, `mask_secret`, `edit_value`,
  `provider_editor`, `handle_tools_command`, `config_picker`,
  `open_stats_dashboard` (lines 1687–2176, ~490 lines)
- All `&mut self` methods; needs `App` split or a trait. Largest win, highest
  risk. Consider a `PickerExt` trait or passing needed fields explicitly.

### 5. Split `handle_key` dispatch
- 996–1289 (~290 lines): pane nav, autocomplete, keymap, paste flood all in
  one match ladder.
- Break into `handle_pane_keys`, `handle_autocomplete_keys`, then fall through
  to input. Keep in mod.rs (it's the core dispatch) but shorter.

## Expected result
- `mod.rs` ≈ 1500–1600 lines: struct + ctor + conversation lifecycle + run loop
  + handle_key + prompt_and_wait + lua command runner.
- New: `editor.rs`, `paste.rs`, `pickers.rs`.

## Out of scope
- No behavior changes. Each step must compile + pass `cargo test` before next.
- No touching `stream/mod.rs` or any Rust policy (Lua-first rule).
