# bone Codebase Deep-Dive Investigation

**Scope**: `protocol` (956 LOC) · `core` (19.2K LOC) · `tui` (12.2K LOC) · Lua defaults · build/CI
**Method**: 10 parallel reviewer subagents, each owning a slice of the tree. ~42K LOC of Rust reviewed end-to-end.
**Verdict date**: 2026-06-28

---

## TL;DR — Is it production-ready?

**No.** Functional and well-architected, but has hard blockers before it's safe to ship as a daemon/remote-service. As a single-user local CLI it mostly works. Specifically:

| # | Blocker | Why it matters |
|---|---|---|
| 🔴 1 | **Path-traversal in every file tool** (`read_file`, `write_file`, `edit_file`) | The model can read `/etc/shadow`, `~/.ssh/id_rsa`, or write anywhere the process can. Only "protection" is the approval gate, with **zero defense-in-depth**. No canonicalization, no workspace-root confinement, no symlink-escape check. |
| 🔴 2 | **No CI runs tests/clippy/fmt/audit** | Releases are tagged and shipped with *zero* automated verification. `.github/workflows/npm-release.yml` only builds binaries on tag push. |
| 🔴 3 | **283 `.unwrap()` / 29 `.expect()` in non-test src** | ~28 of those are on network I/O, DB queries, user/model input, or Lua VM state — they crash the process (and lose the user's session/TUI state). |
| 🔴 4 | **Terminal corruption on any panic** | TUI `run()`/`run_event_pump()` propagate `?` on crossterm I/O errors without a `Drop` guard → raw mode left on, user's shell bricked. |
| 🔴 5 | **Silent error swallowing everywhere** | `.unwrap_or_default()` / `let _ =` / `if let Ok(_)` hide: HTTP error bodies, stderr reader panics, serialization failures, config parse errors, terminal-width=0. Debugging in prod is near-impossible. |

These five are "fix before any v3.0 daemon/remote mode" tier.

---

## 1. Aggregate Signals

| Signal | Count | Notes |
|---|---:|---|
| `.unwrap()` (non-test src) | 283 | ~28 crash-on-real-input; rest are lock-guards/bundled data |
| `.expect()` (non-test src) | 29 | |
| `panic!`/`todo!`/`unimplemented!` (src) | 1 | `tui/src/ui/app/stream/mod.rs:1246` |
| `unsafe` blocks (src) | 2 | both `link(2)` FFI in `shell.rs` — well-justified |
| `.clone()` (src) | 411 | ~15 hot-path sites (history, Vec<ToolCall>, spinner phrases) |
| `Result<_, String>` | 69 | de-facto error type — loses `.source()`/backtrace/kind |
| TODO/FIXME markers | 0 | no tracked debt markers anywhere |
| Files with zero test coverage | ~65 / ~90 | tools/, runtime/, llm/ subsystems basically untested |
| Largest god-object | 2,227 LOC | `tui/src/ui/app/mod.rs` |

---

## 2. Critical Production Issues (ranked, with file:line)

### 🔴 Security — Path traversal / TOCTOU in file tools

| Where | Issue | Fix |
|---|---|---|
| `core/src/tools/read_file.rs:52,74` | `fs::read_to_string`/`fs::read` with **no** path canonicalization. Symlink at `img.png → /etc/shadow` is read + base64'd to the model. | Canonicalize vs a workspace root; reject `..`; open with `O_NOFOLLOW`. |
| `core/src/tools/write_file.rs:31,37-39` | `create_dir_all(parent)` for arbitrary paths; `exists()`→`rename` TOCTOU silently overwrites. | Confine to workspace; `renameat2(RENAME_NOREPLACE)`. |
| `core/src/tools/edit_file/mod.rs:55-62` | read→hash→write with no lock. Concurrent edit is silently clobbered. | `flock` / hold an open fd across read+write. |
| `core/src/tools/shell.rs:93-123,181` | No execution-time allowlist beyond approval gate. `args.classification` is consumed (`let _ =`) and never checked. | Re-verify `classify_command` inside `run_script`; delete dead `classification` field. |
| `core/src/tools/command_policy/mod.rs:250-253` | `trim_matches` only strips shell metacharacters at edges — `$(curl …)` parses as `$(curl` and matches nothing. | Parse shell AST, not prefix/suffix trim. |

### 🔴 Crashes on real input (the worst unwraps)

| Where | What panics | Fix |
|---|---|---|
| `core/src/rpc/codec.rs:86,1009,1014,1016` | `.unwrap().unwrap()` on network read/decode | propagate `Err` |
| `core/src/rpc/mod.rs` (17× `session.lock().unwrap()`) | poisoned mutex crashes daemon | `thiserror` + `?` |
| `core/src/llm/providers/codex.rs:123,420` + `openai_compat/mod.rs:93` | `reqwest::Client::builder().build().unwrap_or_default()` — builder failure silently produces a **timeout-less** client (no connect/read timeout, no pool limits) → hangs forever on dead peers | propagate `Result` |
| `core/src/runtime/driver.rs:405` | `stream.unwrap()` after retry loop | `match` + `break Err` |
| `core/src/tools/read_file.rs:84` | `path.unwrap()` on model-provided JSON | `?` |
| `core/src/ext/ctx.rs:453-454` | `child.stdout/stderr.take().unwrap()` | `.expect("piped via spawn config")` |
| `tui/src/ui/app/stream/mod.rs:1244` | `rx.try_recv().expect(...)` in UI loop | handle `Err(Closed)` |
| `core/src/ext/api.rs:199-274` | Lua-VM `.unwrap()` chain at init | `?` propagate |

### 🔴 Terminal corruption (TUI)

`tui/src/ui/app/mod.rs:606,621` — `event::read()?` propagates out of `run()`; `stream/mod.rs:274` uses `unwrap_or(Event::Key(Null))` to swallow errors. Any I/O error or panic leaves raw mode **on**, corrupting the user's shell. **Fix**: a `TerminalGuard` with `Drop` that calls `disable_raw_mode()` + `LeaveAlternateScreen` in *all* exit paths (incl. `catch_unwind`). Same pattern needed in `fullscreen.rs:73-78` and `editor.rs:21`.

### 🔴 Silent failures (you said you hate these)

| Where | Hidden error |
|---|---|
| `core/src/llm/providers/codex.rs:576` + `openai_compat/mod.rs:496` | `response.text().await.unwrap_or_default()` → HTTP error body becomes `""`. Provider errors become invisible. |
| `core/src/ext/ctx.rs:517` | `stderr_thread.join().unwrap_or_default()` → a panicking stderr reader is hidden, caller sees empty stderr. |
| `core/src/runtime/driver.rs:332` | `terminal::size().unwrap_or(0)` → zero-width terminal → div-by-zero/panic in renderers. |
| `core/src/rpc/mod.rs:199-208` | `to_value(...).unwrap_or_default()` for theme/keymap/config snapshots → null → blank UI, no log. |
| `core/src/config/mod.rs:11` | `serde_yaml::from_str(raw).ok()` → malformed config silently `None`. User's whole config vanishes with no warning. |
| `core/src/ext/catalog.rs:258` | `let _ = fetch_index();` → catalog fetch failures invisible. |
| `core/src/ext/engine.rs:270,278,290` | `if let Ok(...)` dropping Lua sandbox errors → **sandbox can fail open**. |
| `core/src/update_check.rs:52,37` | TLS build error / cache write → swallowed. |

### 🟠 Memory leaks / unbounded growth

| Where | Issue |
|---|---|
| `core/src/llm/providers/codex.rs:566-576` | `emitted_tool_call_ids` / `emitted_reasoning_ids` `BTreeSet`s grow for the entire conversation. Long sessions leak proportional to total tool calls. **Clear on `response.completed`.** |
| `tui/src/ui/app/mod.rs:749` | `events_rx.try_recv()` drains **all** buffered events per tick — a 10k-event sub-agent burst freezes the UI. Cap per-tick (e.g. 100). |
| `core/src/tools/mod.rs:64` | `loaded.registry.clone().register(tool)` → O(n²) clone over the whole HashMap for every Lua tool at boot. |

### 🟠 Concurrency / deadlock

| Where | Issue |
|---|---|
| `core/src/run.rs:72` | Lua `MutexGuard` dropped *before* `handler.call` → other threads can mutate the VM mid-call. |
| `core/src/rpc/mod.rs:430` | same: guard dropped, then stale `mlua::Function` called. |
| `core/src/ext/ctx.rs:25` | `block_in_place` + `Handle::block_on` deadlocks on a current-thread runtime. |
| `core/src/ext/ctx.rs:665-669` | `static KEY_MUTEX` is process-wide; blocks every Lua context **and** holds the VM lock → cascading stalls. |
| `core/src/ext/types.rs:573` | `dispatch_before_turn` builds ctx table whose closures re-enter the same `Arc<Mutex<Lua>>` → deadlock. |
| `core/src/runtime/event.rs:42,96` | `unwrap_or_else(|e| e.into_inner())` recovers from poisoned mutex silently. |
| `tui/src/ui/app/stream/mod.rs:76` | `KeySink.owns_input` latch never released if a tool exits without `ToolResult` → **all keystrokes swallowed for the rest of the turn**. |

### 🟠 Correctness bugs

| Where | Bug |
|---|---|
| `core/src/agent.rs:337-343` | `truncate_str` slices at a byte offset — **panics on multi-byte UTF-8**. Hits any non-ASCII summary >200 bytes. Use `floor_char_boundary`. |
| `core/src/run.rs:101` | `current_dir().unwrap_or_default()` → empty `PathBuf` → all relative ops resolve to `/`. |
| `core/src/llm/providers/codex.rs:190-192` | missing `output_index` collapses multi-tool streams into index 0 — silently drops tool calls. |
| `core/src/session_db.rs:100` | `CREATE VIRTUAL TABLE ... USING fts5` — **hard runtime failure** on distros whose SQLite lacks FTS5. |
| `core/src/session_db.rs:370` | `unchecked_transaction()` in `setup_schema()` can prematurely commit an outer txn. |
| `core/src/tools/registry.rs:155` | `count() > 1` should be `>= 1` — a batch with exactly one stateful call runs parallel and may see stale state. |
| `core/src/tools/command_policy/mod.rs:202` | `sed -i` detection misses `--in-place=SUFFIX`; over-matches `--ignore-file`. |
| `core/src/config/custom.rs:445-483` | `migrate_providers_file` writes to the *same* path it reads — no version guard; re-migrates already-migrated files. |
| `core/defaults/providers.yaml:80-87` vs `pages/providers.yaml` | `wafer` provider silently dropped on upgrade to new page format. |
| `core/src/config/pages/providers.yaml` | hardcoded fictional models (`gpt-5.5`, `google/gemini-3.1-flash-lite`, `GLM-5`) → broken configs for new users. |
| `core/src/tools/edit_file/diff.rs:38` | `TextDiff::from_lines` mishandles `\r\n` → spurious diff lines on Windows. |
| `tui/src/ui/stats.rs:614` | `BackTab` uses `(field+1)%2` — works by accident for 2 fields, wrong semantics. |
| `tui/src/ui/app/mod.rs:700` | `event::poll(..).unwrap_or(false)` on disconnected terminal → busy-spins at 100% CPU forever. |

### 🟡 Security/hygiene (lower tier)

- `core/src/config/mod.rs:168,272` + `custom.rs:115` — config files written `0o644` (world-readable); will later hold API keys. Write `0o600`.
- `core/src/ext/ctx.rs:1014-1040` — `ctx.db.query` allows any SQL starting with `"select"`, including `SELECT 1; DROP TABLE …` (rusqlite executes multiple statements).
- `core/src/llm/providers/openai_compat/mod.rs:347-352` — `stream_options` set only for openai/localhost; DeepSeek/OpenRouter/GLM silently report 0 tokens.
- `core/Cargo.toml:15` / `tui/Cargo.toml:31` — `serde_yaml = { package = "yaml_serde" }` is an **unmaintained fork**. Move to `serde_yml`.

---

## 3. God Objects — Structural Refactors (LOC-neutral but mandatory for maintainability)

Five files are >900 LOC. None *reduce* total LOC by splitting, but each becomes unreadable. The reviewers proposed concrete splits:

### `tui/src/ui/app/mod.rs` — 2,227 LOC → ~12 submodules (~100 LOC each)
`state.rs`, `config.rs`, `events.rs`, `commands.rs`, `approval.rs`, `panes.rs`, `timer.rs`, `autocomplete.rs`, `draw.rs`, `quit.rs`, `lifecycle.rs`, `catalog_setup.rs`.

### `core/src/ext/ctx.rs` — 2,178 LOC → 10 submodules
`ctx/{mod,io,ui,tables,agent,config,usage,state,db,snapshot}.rs`. `mod.rs` becomes an ~80-LOC coordinator.

### `tui/src/ui/app/stream/mod.rs` — 1,310 LOC → 7 submodules
`stream/{keysink,pump,thinking,tick,command,drain,submit}.rs`.

### `core/src/session_db.rs` — 1,176 LOC → 4 submodules
`session_db/{mod,messages,usage,types}.rs`. Critical because `usage_stats_snapshot()` fires 15+ separate SQL queries per call (blocks UI for seconds on power-user DBs).

### `core/src/rpc/mod.rs` — 1,053 LOC → 3 submodules
`rpc/{hub,daemon,mod}.rs`.

### `core/src/config/custom.rs` — 792 LOC → 5 submodules
`config/custom/{types,load,save,migrate,mod}.rs`. (custom.rs also owns 3 migrations + UI cycle logic — too many concerns.)

### `tui/src/ui/stats.rs` — 984 LOC → 4 submodules; `tui/src/ui/render/bottom_pane.rs` — 886 LOC → 5 submodules.

---

## 4. LOC-Reducing Refactors (actual deletions)

These are the high-value cleanups that *remove* lines.

| Where | Change | Est. saved |
|---|---|---:|
| `core/src/ext/mod.rs:250-316` | 3 identical `seed_default_lua_*` fns → 1 generic `seed_default_lua(dir, bundled, allow, force)` | **~50** |
| `core/src/llm/providers/{codex,openai_compat}` | Extract shared `llm::stream_utils` (`PartialToolCall`, `flush_partial_tool_calls`, `ThinkParser`, SSE loop, error-body parse, client build). Currently one-directional import codex→openai_compat. | **~150** |
| `core/src/tools/mod.rs:32-37` | Merge `dynamic_display` / `dynamic_safety` / `dynamic_state` parallel HashMaps → `HashMap<String, ToolMeta>` | **~40** |
| `core/src/tools/registry.rs:21,96`; `mod.rs:64` | `register(&mut self)` + `Arc<ToolHandler>` instead of clone-and-return. Kills O(n²) boot clone. | **~30** + perf |
| `core/src/tools/write_atomic.rs:9-16` | Replace manual PID+nanos temp-file with `tempfile::NamedTempFile` (+ add `sync_data()` + dir `fsync` for real durability) | **~15** + correctness |
| `core/src/llm/prompts.rs:5-63` | `system_prompt` & `headless_agent_system_prompt` are ~90% identical → one `format_prompt(first_line, memory)` | **~35** |
| `core/src/agent.rs:280-295` | `emit_event` manual per-variant match → derive `Serialize` on a `CliEvent` type | **~13** |
| `core/src/runtime/driver.rs:530-588` | token-usage reporting duplicated ~60 LOC → `fn emit_token_usage(...)` | **~55** |
| `core/src/runtime/driver.rs:395-835` | `run_to_outcome_inner` is ~440 LOC → extract `consume_stream()` + `execute_tools()` | **~140** |
| `core/src/config/providers_config.rs:123-154` | `from_nested()` manual YAML deser duplicates `#[derive(Deserialize)]` → `serde_yaml::from_value` + `deny_unknown_fields` | **~25** |
| `tui/src/ui/{stats,jobs_pane,render/bottom_pane,render/messages,render/markdown}` | Dedup `compact_number`/`format_tokens`, key-hint footer, prefix-wrapping, width-truncation into shared `ui/util.rs` | **~200** |
| `tui/src/ui/app/mod.rs` + `stream/mod.rs` | Pane-nav logic duplicated verbatim (~50 LOC) in `handle_key` & `drain_keys` | **~50** |
| `tui/src/ui/app/mod.rs:1022-1026,1900` | `apply_view_diffs()` always returns `false` (dead); `shell_quote` is single-use | **~10** |
| `core/src/ext/types.rs:480-540` | `lua_value_to_json` duplicates `mlua::LuaSerDeExt::from_value` | **~60** |
| `core/src/ext/types.rs:741-790` | `guard_with_bone`/`dispatch_event_inner`/`create_event_ctx` are dispatch logic, not types → move to `ext/dispatch.rs` | **~50** moved |
| `core/src/ext/ctx.rs:1862-1877` | `StreamCallbacks::from_opts` 7 field calls → `HashMap<String, Function>` | **~30** |
| `core/src/llm/providers/codex.rs:677-745` | `codex_debug_*` (68 LOC) → `#[cfg(feature="codex_debug")]` | **~63** in release |
| `core/src/ext/ctx.rs:1081-1082,921,944` | cache parsed config + session-db connection (reopened every call) | perf (not LOC) |
| **Estimated total removable** | | **~900-1000 LOC** |

Plus the protocol cleanups (`deserialize_vec_or_empty_map`, `CommandAction` pseudo-oneof) — minor.

---

## 5. Cross-Cutting Concerns

### Error handling
- **No unified error type.** 69× `Result<_, String>` (loses `.source()`/backtrace/kind), 22× `Result<_, mlua::Error>`, 9× `LlmError`, ~4× `io::Error`, 147 panics. Only `llm/` has a proper enum (`LlmErrorKind`).
- `crate::util::errstr` is a `Display→String` stopgap used ~50×.
- **Recommendation**: introduce `bone_core::Error` (thiserror) wrapping `LlmError`, `io::Error`, `mlua::Error`, ad-hoc strings; convert the `Result<_,String>` family.

### Test coverage (worst gaps — safety & user-data paths)
**Zero tests**: `tools/shell.rs`, `tools/read_file.rs`, `tools/write_file.rs`, `tools/write_atomic.rs`, `tools/edit_file/*`, `tools/approval.rs`, `tools/command_policy/mod.rs`, `runtime/conn.rs`, `runtime/session.rs`, `llm/prompts.rs`, `llm/provider.rs`, `ext/engine.rs`, `ext/lua_tool.rs`, `session_sink.rs`, `config/providers_config.rs`, `run.rs`, `tui/src/ui/app/mod.rs`.

Only `core/src/ext/` has decent coverage. **The security-critical tool layer is essentially untested.**

### CI / build
- `.github/workflows/npm-release.yml` is the *only* workflow, runs only on `v*` tags, and **does not run `cargo test`, `clippy`, `fmt`, or `cargo audit`**.
- No MSRV declared (edition 2024 ⇒ Rust 1.85+).
- `arboard` with `wayland-data-control` requires Wayland dev headers at build time — breaks on minimal containers.
- Duplicate dep declarations (`tokio`, `mlua`, `serde_yaml`) across core+tui Cargo.tomls → should be `[workspace.dependencies]`.
- `Cargo.lock` has duplicate `png`, `unicode-width`, `bitflags`, `hashbrown` (semver splits) — review.
- Release profile missing `panic = "abort"` (would also force addressing the unwrap culture).

### Lua defaults
- No `xpcall` anywhere (8 `pcall`s, no error handlers).
- `commands/compact.lua:55` relies on global `utf8` without explicit require.
- `commands/config.lua` discards edits silently on Esc.
- `lib/ui/menu.lua:32-37` `utf8_chars` allocates a full table per keystroke.
- Zero Lua test files.

### Perf smells (hot-path clones)
1. `core/src/ext/ctx.rs:715` — full `Vec<ChatMessage>` history clone per tool invocation → `Arc<History>`.
2. `core/src/runtime/driver.rs:846` — `Vec<ToolCall>` clone (large JSON args) → drain/move.
3. `core/src/ext/lua_tool.rs:319-324` — 6-field clone storm per tool call.
4. `tui/src/ui/app/mod.rs:1267` — spinner `phrases: Vec<String>` cloned every render frame (80-150ms).
5. `core/src/runtime/session.rs:68` — transcript cloned for driver → `Arc<Vec<ChatMessage>>`.
6. `core/src/llm/providers/markdown.rs:379-410` — `words_from_spans` per-character `Span` allocation.

---

## 6. Recommended Action Plan (priority order)

### Phase 0 — Stop-the-bleeding (before any v3/daemon release)
1. **Workspace confinement in file tools**: canonicalize + workspace-root check + `O_NOFOLLOW`/`RENAME_NOREPLACE` + `flock` on edit. (`read_file`, `write_file`, `write_atomic`, `edit_file`)
2. **Real durability in `write_atomic`**: `sync_data()` before rename + parent-dir `fsync`.
3. **Drop-in `Drop` guard for terminal state** (TUI): `TerminalGuard` restoring raw mode + alt-screen on panic/`?`/early-return. Apply in `run()`, `run_event_pump()`, `fullscreen.rs`, `editor.rs`.
4. **CI workflow**: add `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo audit` on every PR + a Linux/macOS/Windows matrix. Gate releases on green.
5. **Sweep the ~28 crash-on-input unwraps** → `?` propagation (prioritize rpc/codec, rpc/mod session.lock, llm provider client builds, driver stream, read_file path, ctx child.stdout).
6. **Replace silent fallbacks** in the 8 worst spots (codex/openai error bodies, ctx stderr join, driver terminal::size, rpc snapshots, config yaml parse, engine sandbox errors) → at minimum `eprintln!`/`tracing::warn`.

### Phase 1 — Correctness
7. `truncate_str` UTF-8 boundary fix.
8. `session_db` FTS5 fallback (try/catch → LIKE query).
9. `migrate_providers_file` version guard; restore `wafer` to `pages/providers.yaml`; replace fictional model names with placeholders.
10. Clear `emitted_*_ids` sets on `response.completed`.
11. Fix `registry.rs:155` `>1` → `>=1`; `command_policy` sed detection; `stream/mod.rs` `KeySink.clear_owner()` on `TurnComplete`.

### Phase 2 — Structural (god-object splits)
12. `tui/src/ui/app/mod.rs` → 12 submodules.
13. `core/src/ext/ctx.rs` → 10 submodules.
14. `core/src/session_db.rs` → 4 submodules (and fix the 15-query `usage_stats_snapshot`).
15. `core/src/rpc/mod.rs` → 3 submodules.
16. `core/src/config/custom.rs` → 5 submodules.
17. `tui/src/ui/stats.rs`, `render/bottom_pane.rs` splits.

### Phase 3 — LOC reduction (~900-1000 LOC)
18. Shared `llm::stream_utils` (merge codex + openai_compat).
19. `seed_default_lua_*` generic.
20. `ToolMeta` merge; `Arc<ToolHandler>`; mutable `register`.
21. `tempfile::NamedTempFile`.
22. TUI util dedup (number fmt, key-hints, wrapping, truncation).
23. `lua_value_to_json` → `LuaSerDeExt`.
24. `emit_token_usage` extraction; `consume_stream`/`execute_tools` extraction.

### Phase 4 — Hygiene
25. Introduce `bone_core::Error` (thiserror); migrate `Result<_,String>`.
26. Replace `yaml_serde` fork with `serde_yml`.
27. Move duplicate deps to `[workspace.dependencies]`; declare MSRV.
28. Add tests for the security-critical tool layer (`shell`, `read_file`, `write_file`, `edit_file`, `command_policy`, `approval`).

---

## Appendix: Reviewer Coverage Matrix

| # | Area | LOC | Files | Issues | Refactors |
|---|---|---:|---:|---:|---:|
| 1 | Foundations + small crates | 2,584 | 20 | 12 | 8 |
| 2 | ext/ctx.rs + types.rs + mod.rs + engine.rs | 3,962 | 4 | 17 | 16 |
| 3 | ext/ api,api_ui,jobs,loader,lua_tool,ops_*,snapshots,inbox,catalog | ~3,500 | 12 | — | — |
| 4 | llm/* | 1,709 | 7 | 16 | 9 |
| 5 | tools/* | 2,222 | 12 | 14 | 6 |
| 6 | runtime/* + rpc/* + session_db.rs | 3,580 | 9 | 16 | 7 |
| 7 | config/* + yaml | 1,925 | 10 | 19 | 8 |
| 8 | tui main/lib/ui/app/* | 4,300 | 7 | 18 | 14 |
| 9 | tui render + misc UI | 9,360 | 22 | 14 | 11 |
| 10 | Cross-cutting (unwraps, errors, tests, deps, lua) | — | — | 14 | 7 |

(Full per-file detail with quoted snippets lives in the reviewer reports under `/tmp/bone-jobs/`.)
