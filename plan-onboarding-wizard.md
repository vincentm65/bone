# First-Launch Onboarding Wizard

**STATUS: IMPLEMENTED.** `cargo build` clean, `cargo test` 126 passing
(+2 new seed tests). Deviations from the original plan:
- The selection file `~/.bone-rust/.setup.json` **doubles as the onboarding
  marker** (no separate `.onboarded` file). `needs_onboarding()` = no `init.lua`
  AND no selection file, so existing users upgrading are never forced through it.
- Init step gained a **"Keep current"** option when an `init.lua` already exists
  (so re-running `/setup` never clobbers a customized init).
- `/setup` inside the TUI shows a "restart to load changes" notice (seeded
  tools/commands don't hot-reload mid-session).

---


Goal: a one-time, fullscreen (tmux-popup style) setup wizard that teaches bone
and lets the user choose **which default tools** and **which default commands**
get seeded into their `~/.bone-rust/` copy, plus whether `init.lua` is
**auto-populated** (banner + a live subagent) or **blank**. Re-runnable via
`/setup`.

## Decisions (locked)
- **Wizard is Rust**, mirroring `/stats` (`src/ui/stats.rs`). It must run before
  the Lua runtime boots and must gate `seed_all()`, so it can't be a Lua flow.
- **Populated init.lua** = current banner function + a real registered subagent
  (live immediately). **Blank** = minimal header comment only.
- **Marker + `/setup`**: write `~/.bone-rust/.onboarded` on completion; auto-run
  once when absent; `bone setup` / `/setup` re-runs anytime.

## UX model (copy `/stats` exactly)
- Launch: inside the TUI, `/setup` uses `tmux display-popup -E '<exe> setup'`
  (same guard as `open_stats_dashboard`, `src/ui/app/mod.rs:1994`), with an
  in-process `EnterAlternateScreen` fallback.
- Cold first-run (from `main()`, before any TUI/alt screen): run in-process
  alt-screen wizard directly (whole terminal is free — no popup needed).
- New entry point `bone setup` in `main.rs` runs the same wizard standalone.

## Wizard steps (ratatui, multi-step)
1. **Welcome / teach** — short intro: what tools/commands/init.lua are, pointer
   to `/customize` for everything else. (Reuse the framing from
   `defaults/lua/commands/customize.lua`.)
2. **Pick tools** — multi-select over the 5 defaults (ask_user, cron, subagent,
   task_list, web_search). All checked by default.
3. **Pick commands** — multi-select over the 9 defaults (compact, config,
   customize, goal, history, memory, review, shotgun, usage). All checked.
4. **init.lua** — single select: Auto-populated (banner + subagent) / Blank.
5. **Confirm** — summary of what will be written, then write + marker.

Each row shows a short description parsed from the file's `description = "..."`
field (or leading `--` comment) in the bundled content.

## Implementation

### 1. Selective seeding (`src/ext/mod.rs`)
- Change `seed_default_lua_tools` / `seed_default_lua_commands` to take an
  `allow: Option<&HashSet<String>>`. `None` ⇒ seed all (preserve upgrade
  behavior); `Some(set)` ⇒ only filenames in `set`.
- Add small helper to list `(filename, description)` pairs from
  `DEFAULT_LUA_TOOLS` / `DEFAULT_LUA_COMMANDS` for the picker.

### 2. Selection-aware seed entry (`src/config/mod.rs`)
- `seed_all()` keeps current behavior (all). Add `seed_all_with(&SetupSelection)`
  that forwards the tool/command allow-sets. Libs always seed fully (menus etc.
  are infra). Add `onboarded_marker_path()` + `is_onboarded()`.

### 3. init.lua templates (`src/ext/engine.rs`)
- Keep `DEFAULT_INIT_LUA` (banner) as the "populated" base; append a registered
  subagent block (e.g. a `researcher` via `bone.register_subagent`).
- Add `BLANK_INIT_LUA` (header comment only).
- Wizard writes the chosen template to `init.lua` directly, so `run_init`'s
  "create if missing" branch never fires (it just runs the file). Blank choice
  therefore won't get re-populated with the banner.

### 4. The wizard (`src/ui/setup.rs`, new — model on `stats.rs`)
- `RawModeGuard`, `EnterAlternateScreen`, ratatui `Terminal<BoneBackend>`, a
  step state machine, key handling (↑/↓, space toggle, enter advance, esc back).
- Returns a `SetupSelection { tools, commands, init: Populated|Blank }`.
- A `run()` that drives the loop and performs the writes + marker on confirm.

### 5. Wiring (`src/main.rs`)
- Add `bone setup` subcommand → `ui::setup::run()` (standalone).
- In `main()`, replace the unconditional `seed_all()` for the **normal TUI path**
  with: if `!is_onboarded()` ⇒ run wizard (which seeds by selection + writes
  marker); else `seed_all()`. Non-TUI paths (`run`, `serve`, `stats-popup`,
  `install`) keep calling `seed_all()` so headless/daemon never blocks on a UI.

### 6. `/setup` command
- Add a `/setup` command that triggers the tmux-popup launch (mirror
  `open_stats_dashboard`); ignores the marker (explicit re-run).

## Files touched
- `src/ext/mod.rs`            — selective seed fns + catalog helper
- `src/config/mod.rs`         — `seed_all_with`, marker helpers
- `src/ext/engine.rs`         — populated/blank init.lua templates
- `src/ui/setup.rs`           — new wizard (model on stats.rs)
- `src/ui/mod.rs`             — expose `setup` module
- `src/main.rs`               — `bone setup` entry + first-run gate
- `src/ui/app/mod.rs` (+ commands) — `/setup` launcher
- tests — selective-seed unit test; marker logic

## Verification
- `cargo build` / `cargo test`.
- Fresh run: `rm -rf ~/.bone-rust` (after backup) → launch → wizard appears,
  pick subsets → confirm only chosen files exist + `.onboarded` present.
- `/setup` re-runs inside a tmux session as a popup.
- Blank init choice stays blank across restarts (run_init doesn't overwrite).

## Out of scope (note for later)
- Provider/model/auth selection during onboarding (separate flow).
