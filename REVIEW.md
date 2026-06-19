# Code Review: Unstaged Changes

## PRIORITY 1 — Bugs

### 1. `src/ui/app/mod.rs` — False success message on wizard cancel

**Lines 2060–2083** (function `open_setup_wizard`):

```rust
if !ran {
    let result = crate::ui::setup::run();
    self.force_redraw(term)?;
    if let Err(err) = result {
        return self.show_reply(format!("Setup wizard failed: {err}"), term);
    }
}

self.show_reply(
    "Setup saved to ~/.bone-rust/. Restart bone to load the new tools and commands."
        .to_string(),
    term,
)
```

`setup::run()` returns `Ok(true)` on completion and `Ok(false)` on cancel (user pressed Esc). The error check only catches `Err`, so `Ok(false)` falls through and shows "Setup saved… Restart bone…" even when the user cancelled and nothing was saved.

**Fix**: Check both the error case AND the boolean. Only show the success message when `setup::run()` returned `Ok(true)`.

---

### 2. `src/ext/mod.rs` — Deselected tools survive on disk and get loaded on restart

**Lines 169–182** (`seed_default_lua_tools`) and **line 289** (`run_lua_files`):

The `allow` filter in `seed_default_lua_tools` only controls which bundled files are *written* to disk — it never removes files. When a user re-runs `/setup` and deselects previously-selected tools:

1. `apply_onboarding()` calls `seed_default_lua_tools(dir, Some(&filtered_set))` — only writes files in the set that don't exist yet (which is none, since they were already seeded at startup).
2. Deselected Lua files remain on disk.
3. On restart, `run_lua_files()` in `loader.rs:84-87` loads **all** Lua files from the directory, regardless of the persisted selection. The selection filter is effectively ignored on re-runs.

**Fix**: Either (a) delete files not in the allow set when seeding, or (b) filter which files `run_lua_files` loads based on the persisted selection.

---

### 3. `src/ui/app/mod.rs` — tmux popup for setup has no effect on running app

**Lines 2057–2076** (function `open_setup_wizard`):

When running inside tmux, the wizard spawns `bone setup` in a tmux popup. The subprocess writes the selection file and init.lua, then exits. The main process continues and shows the "Restart bone" message. However, the main process's Lua state still holds all tools from the previous startup — the subprocess's seeding changes don't propagate back.

For the non-tmux path (same bug as #1 above, plus): `apply_onboarding` is called, which writes the selection file and calls `seed_default_lua_tools` with the filtered set. But `seed_default_lua_tools` only writes files that don't exist yet — so the running Lua state is NOT updated. Old tools persist in memory.

**Fix**: At minimum, document that `/setup` changes only take effect after restart. The message already says this, but the inconsistency between the tmux and non-tmux paths (one uses a subprocess, the other modifies config inline) is confusing and both have the stale-state problem.

---

## PRIORITY 2 — Potential bugs / edge cases

### 4. `src/ui/render/bottom_pane.rs:839-840` — Division by zero risk (clippy warning)

```rust
let cycle = if status_info.spinner_text_speed_ms > 0 {
    (status_info.spinner_elapsed_ms / status_info.spinner_text_speed_ms)
        as usize
```

The `> 0` guard prevents a divide-by-zero because `spinner_text_speed_ms` defaults to 0 (the else branch is used). However, this is fragile: if a future code path sets `spinner_text_speed_ms` to `0` and the `> 0` check is accidentally removed or the logic is refactored, a panic becomes possible. Use `checked_div` or restructure to make the zero case explicitly unreachable.

---

### 5. `src/config/custom.rs` — `backfill_status_fields` doesn't migrate type changes

**Lines 756–777**:

```rust
fn backfill_status_fields() {
    ...
    for seed_field in seed.fields {
        if !status.fields.iter().any(|f| f.key == seed_field.key) {
            status.fields.push(seed_field);
            changed = true;
        }
    }
    ...
}
```

This only adds fields whose `key` is absent. If a field's type or options change between versions (e.g., a field goes from `Bool` to `Enum`), the existing field definition is left untouched. The user sees the old field schema while the code tries to read it with the new type assumptions. Currently not triggered because all new fields have unique keys, but it's a ticking bomb for future schema migrations.

**Fix**: Add a versioning scheme or compare more than just `key` (e.g., also check `field_type` and `options`).

---

### 6. `src/config/pages/status.yaml` — `status_spinner_text_rotate` is type `bool` but not in `STATUS_TOGGLE_KEYS`

The YAML declares:
```yaml
- key: status_spinner_text_rotate
  label: "Rotate spinner text"
  type: bool
  default: true
```

But `STATUS_TOGGLE_KEYS` (in `config/mod.rs:97-110`) does NOT include `status_spinner_text_rotate`. It's handled explicitly as a string comparison (`!= "false"`). This means the `/config` UI treats it as a Bool toggle (reads the YAML schema), but the runtime code reads it as a raw string. If the config UI stores the value as a YAML boolean (`true`/`false`), `get_value` returns `"true"`/`"false"`, which works with `!= "false"`. However, if the YAML serialization ever changes (e.g., to `yes`/`no` or the field type changes), the string comparison would break silently.

---

## PRIORITY 3 — Quality / maintenance concerns

### 7. `src/ui/setup.rs:310-324` — `pad()` obfuscates layout arithmetic

```rust
fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
}
```

Used inconsistently: `draw_welcome`, `draw_init`, and `draw_confirm` all call `pad()` on the area they receive, while `draw_list` also calls `pad()` on its area. But the body chunks already include the padding from the top-level `draw` layout. The `pad` function subtracts 1 from y and height, but the body area's `y` already accounts for the header. This double-padding can cause content to be cut off at the bottom when terminal height is small. Consider using a single `Block` with `Padding` instead.

---

### 8. `src/ui/app/mod.rs` — `timer_elapsed_ms()` returns 0 when `turn_start` is `None`

```rust
pub(crate) fn timer_elapsed_ms(&self) -> u64 {
    let Some(start) = self.turn_start else {
        return 0;
    };
    ...
}
```

Used by `stream_status_info` to set `spinner_elapsed_ms`. When displayed outside a turn (e.g., in the approval pane or idle state), `spinner_elapsed_ms` is 0, so the spinner always shows frame 0. The old `spinner_tick` counter would at least show whatever frame it last advanced to. This is a visible regression: when idle after a turn, the spinner position becomes static frame 0 instead of its last position. Not a crash, but a cosmetic issue.

---

### 9. `src/ui/app/mod.rs:1104` — `stream_status_info_with_token_stats` hardcodes `spinner_elapsed_ms: 0`

```rust
pub(crate) fn stream_status_info_with_token_stats(...) -> StatusInfo {
    ...
    StatusInfo {
        ...
        spinner_elapsed_ms: 0,
    }
}
```

This function is `pub(crate)` and is used directly by `stream_status_info()` (on `App`), which immediately overrides `spinner_elapsed_ms` with `self.timer_elapsed_ms()`. But if any other caller calls this function directly expecting animated spinners, they'll get a frozen frame. Consider moving the override into this function (passing the elapsed ms as a parameter) to prevent future misuse.

---

### 10. `src/config/mod.rs:131-135` — `seed_base()` called twice during fresh onboarding

In `main.rs`:
```rust
bone::config::seed_base();
bone::ui::setup::run()?;    // apply_onboarding calls seed_base() again
bone::config::seed_all_with_persisted();  // calls seed_base() a third time
```

`seed_base()` is idempotent, so this is harmless but wasteful. More importantly, `apply_onboarding` (called from the wizard) writes the selection file and seeds tools, then immediately `seed_all_with_persisted` reads the selection file and seeds tools again — two sequential write-and-read cycles that could be consolidated.

---

### 11. `src/ui/app/mod.rs:2052` — `shell_quote` has no path escaping for tmux

```rust
let cmd = format!("{} setup", shell_quote(&exe.to_string_lossy()));
```

`shell_quote` wraps the binary path in single quotes. This works for most paths but fails if the path contains a single quote (e.g., `/home/user/my'bone`). The resulting shell command would break. Consider escaping embedded single quotes or using a proper shell-escape library.

---

## Summary

| # | File | Severity | Issue |
|---|------|----------|-------|
| 1 | `src/ui/app/mod.rs` | **BUG** | Success message shown even on wizard cancel |
| 2 | `src/ext/mod.rs`, `src/ext/loader.rs` | **BUG** | Deselected tools survive restart |
| 3 | `src/ui/app/mod.rs` | **BUG** | `/setup` running state is stale after wizard |
| 4 | `src/ui/render/bottom_pane.rs` | **BOUNDS** | Fragile zero-division guard on `spinner_text_speed_ms` |
| 5 | `src/config/custom.rs` | **MAINT** | `backfill_status_fields` ignores field type/options changes |
| 6 | `src/config/mod.rs` | **MAINT** | `status_spinner_text_rotate` bypasses bool toggle path |
| 7 | `src/ui/setup.rs` | **COSMETIC** | Double-padding on small terminal heights |
| 8 | `src/ui/app/mod.rs` | **COSMETIC** | Spinner frozen at frame 0 outside turn |
| 9 | `src/ui/app/mod.rs` | **MAINT** | `spinner_elapsed_ms: 0` hardcoded in public helper |
| 10 | `src/main.rs` | **PERF** | `seed_base()` called 3x during fresh onboarding |
| 11 | `src/ui/app/mod.rs` | **SEC** | `shell_quote` doesn't escape embedded single quotes |
