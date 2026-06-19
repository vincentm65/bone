# Configurable Thinking Spinner + Variable Text

Goal: move spinner frames + rotating "thinking" text into a Lua defaults file,
pipe them into the Rust renderer as the live thinking animation, and let the
user pick style / text / speed from `/config` → Status page.

**STATUS: COMPLETE (all 10 steps done).**

## Architecture decision

- **Data in Lua, rendering in Rust** (per Lua-first principle).
  Spinner + text presets live in a seeded Lua lib. Rust snapshots them at boot.
- **Lua owns presets; config owns the user's current selection.**
  Boot snapshot = the registry (style/text tables). Runtime config = which one
  is active + speed override. This mirrors the existing theme/keymap snapshot
  pattern exactly.
- No new field type, no new view-model channel. Reuse `LuaConfigSnapshot`.
- **Elapsed-time frame indexing** via `turn_start` epoch (replaces tick-count).

---

## Implementation (complete)

### Step 1 ✅ — Lua defaults file
`defaults/lua/lib/ui/spinners.lua` with 8 spinner styles (braille, triangle,
pipe, kaomoji, typing, waveline, dots_text, progblock) + 3 text presets
(thinking, pondering, processing). Each spinner has natural frame speed.

### Step 2 ✅ — Boot snapshot collects the registry
`src/ext/snapshots.rs`: `SpinnerPreset`/`TextPreset` types,
`parse_spinner_presets`/`parse_text_presets`/`collect_presets`.
`collect_config_snapshot` in `loader.rs` collects presets via
`require("ui.spinners")` after the config branch. `LuaConfigSnapshot` gained
`spinners`/`texts` fields.

### Step 3 ✅ — UserConfig holds resolved registry
`src/config/mod.rs`: `spinner_styles`/`spinner_texts` on `UserConfig`,
populated in `apply_lua_config_snapshot`.

### Step 4 ✅ — Config page fields
`src/config/pages/status.yaml`: `status_spinner_style` (enum),
`status_spinner_text` (enum), `status_spinner_speed` (number, 0 = default).

### Step 5 ✅ — Selection fields on UserConfig
`src/config/mod.rs`: `spinner_style`/`spinner_text`/`spinner_speed` fields,
populated in `apply_custom_configs` from the status page values.

### Step 6 ✅ — StatusInfo spinner fields
`src/ui/render/mod.rs`: added `spinner_frames`, `spinner_speed_ms`,
`spinner_texts`, `spinner_elapsed_ms` to `StatusInfo`.

### Step 7 ✅ — Resolve selection in builder
`src/ui/app/mod.rs::stream_status_info_with_token_stats`: resolves
selected style's frames + speed (override or default) + selected text's
phrases. `timer_elapsed_ms()` method added for raw elapsed ms.

### Step 8 ✅ — Renderer rewrite
`src/ui/render/bottom_pane.rs`: elapsed-time frame indexing
`(elapsed_ms / speed) % frames.len()` + text rotation per full spinner cycle.

### Step 9 ✅ — Dead code removal
Removed: `SPINNER` const, `spinner_tick` field, `tick` param from
`draw_bottom_pane_with_tick`. `tick_spinner` now just forces a redraw
(elapsed time drives the animation).

### Step 10 ✅ — Test fix
`tests/bottom_pane_test.rs`: added new StatusInfo fields to test fixture.

## Files touched
- `defaults/lua/lib/ui/spinners.lua`        (new)
- `src/ext/snapshots.rs`                    (+2 presets on snapshot)
- `src/config/mod.rs`                       (registry + selection on UserConfig)
- `src/config/pages/status.yaml`            (3 fields)
- `src/ui/render/mod.rs`                    (drop const, StatusInfo fields)
- `src/ui/render/bottom_pane.rs`            (elapsed frame + variable text)
- `src/ui/app/mod.rs`                       (resolve selection → StatusInfo)
- `tests/bottom_pane_test.rs`               (StatusInfo fixture update)

## Verification
- `cargo build` ✅
- `cargo test` — 124 passed, 0 failed ✅
- Runtime: `bone` → trigger a turn, confirm configured spinner + rotating text
- `/config status` → cycle style/text/speed, confirm live effect
