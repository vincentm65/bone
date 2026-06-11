# Sub-Agent Feature — Phased Implementation Plan

## Summary

User-managed sub-agents registered in `init.lua`, shown idle in the Live (bottom) pane, dispatched in parallel by the main agent via **one new tool** (`subagent`). Jobs run as background tokio tasks (non-blocking). When jobs finish and the TUI is idle, results are **auto-injected** as a new turn so the main agent wakes up — no polling by the agent.

Decisions made with user:
- Result delivery: **auto-injection** (no agent-side poll/wait loop)
- Live pane: **TUI tick polls a job-registry version counter; Lua renders the pane**
- New Rust primitives under **`ctx.agent`**: `spawn` + `jobs`

Logic split: Rust provides only the job registry, spawn primitive, and tick hook (~300 lines). All policy/rendering/dispatch logic lives in Lua.

## Key verified facts

- Boot order (`src/ext/loader.rs:21-90`): engine create → `init.lua` → defaults `lua/tools/*.lua` → collect tools. So `bone.register_subagent` must be installed by Rust at engine creation; `subagent.lua` runs after `init.lua` and can embed registered agents in its tool description.
- `ctx.agent.run` is blocking (`src/ext/ctx.rs:862-909`); `run_agent(request) -> Result<AgentResponse, String>` (`AgentResponse { content }`). Each background `run_agent` boots its **own Lua VM** — no contention with the TUI VM.
- TUI streaming runs inline in the main loop (`src/ui/app/stream/mod.rs:138` `submit_user_turn(text, display_text, term)`); when the `run()` loop ticks (`src/ui/app/mod.rs:295-323`), `self.streaming` is always false → injecting from the tick is re-entrancy-safe.
- Pane plumbing: `PanePage::from_json` / `upsert` / `remove` (`src/ui/pane_page.rs`). Pages start empty at boot.
- Top-level Lua tools run inside `spawn_blocking` (`src/ext/lua_tool.rs:244`) where `Handle::current()` works (already used by `ctx.agent.run`).

---

## Phase 1 — Core: job registry + `ctx.agent.spawn` / `ctx.agent.jobs` (Rust)

Goal: background, non-blocking agent runs callable from Lua, with results queryable. Independently testable without UI.

**1a. New module `src/ext/jobs.rs`** (~130 lines), `pub mod jobs;` in `src/ext/mod.rs`:

```rust
pub enum JobStatus { Running, Done, Error }          // as_str(): "running"|"done"|"error"
pub struct Job { id, agent, task, status, result: Option<String>,
                 started_at: u64, finished_at: Option<u64>, consumed: bool }
pub struct JobRegistry { jobs: Mutex<Vec<Job>>, version: AtomicU64, next_id: AtomicU64 }
pub fn registry() -> &'static JobRegistry            // OnceLock (global: jobs are app-lifetime, user-owned)
```

Methods: `version()`, `create(agent, task) -> String` (id `"job-N"`, bumps version), `complete(id, Result<String,String>)` (bumps), `complete_with_tokens(id, Result, sent, received)` (same + writes token counts into Job), `update_tokens(id, sent, received)` (updates running job's token counts), `snapshot() -> serde_json::Value`, `take_finished_unconsumed() -> Vec<Job>` (marks consumed, bumps). Plus `MAX_INJECT_CHARS: usize = 16_000` and char-boundary-safe `truncate_for_injection()`. In-module unit tests.

**1b. `src/ext/ctx.rs` — extend `add_agent_table()`** (line 851, after `run_stream`):

- `ctx.agent.spawn(prompt, opts?) -> {ok, id?, error?}`:
  - Reject if `agent_depth > 0` — sub-agents can't spawn background jobs (their results would inject into the main conversation); blocking `run` remains available to them.
  - Reuse `parse_agent_opts()`; also read `opts.agent` (registered name, default `""`).
  - `Handle::try_current()` → `registry().create(...)` → shared `Arc<AtomicU64>` for token counts → `handle.spawn(async { timeout(run_agent(request)) → registry().complete_with_tokens(id, outcome, final_sent, final_received) })`.
  - Do **not** wire `cfg.cancelled` — jobs survive Esc (user-managed lifecycle). `agent_depth + 1` in the request.
- `ctx.agent.jobs() -> array` via `lua.to_value(registry().snapshot())`.

**Phase check**: `jobs.rs` unit tests pass; a probe Lua tool in a test can spawn and observe job completion (Error path when no provider configured).

---

## Phase 2 — Registration: `bone.register_subagent` (Rust, tiny)

Goal: users can declare sub-agents in `init.lua`.

- `src/ext/ops_tools.rs`: `setup_register_subagent(lua, bone)` (~25 lines, mirrors `setup_register_tool`): creates `bone._subagents = {}`; validates `name`/`description` non-empty, warns + skips duplicates, pushes entry `{name, description, system_prompt?, provider?, model?, approval?}` as-is.
- `src/ext/engine.rs::create_engine`: wire next to `setup_register_tool` (~line 57) so it exists **before** `init.lua` runs.

**Phase check**: test boots with an `init.lua` calling `bone.register_subagent` twice; `bone._subagents` has 2 entries.

---

## Phase 3 — The `subagent` tool + pane renderer (Lua — the bulk of the logic)

Goal: main agent can discover and dispatch; pane rendering defined.

New `defaults/lua/tools/subagent.lua` (auto-embedded by build.rs, seeded to `~/.bone-rust/lua/tools/`):

- Read `bone._subagents`; if empty → `return` (no tool, no pane, zero overhead).
- `render_pane(jobs)` → `{source="subagents", title="Agents (N)", visible_rows, lines=...}`; per agent latest-job status: `○ idle` / `◐ running <task> (elapsed)` / `✓ done` / `✗ error` (elapsed via `os.time() - started_at`).
- Export hook for Rust: `bone._subagents_render = function(jobs) return render_pane(jobs) end` (pure function; jobs snapshot passed in).
- Register one tool:
  - `name="subagent"`, `safety="read_only"`; description dynamically lists registered agents and states: dispatch is non-blocking, results arrive automatically in a later turn, **never poll or wait**. `display.show = true` (visible in message timeline).
  - `parameters`: `{action: "dispatch"|"status", tasks: [{agent, task}]}`.
  - `dispatch`: per task — unknown agent → error line; agent already running → `REJECTED` (no queueing); else `ctx.agent.spawn(task, {agent=name, system_prompt=..., provider=..., model=..., approval=...})`. Returns report + pane in JSON envelope. Token counts shown in running status as `in/out`.
  - `status`: snapshot summary + pane. No waiting.

Note: sub-agent VMs also load this file, but `spawn` rejects at depth > 0 — nesting/creation rules hold.

**Phase check**: boot with registered agents → `tools.definitions()` contains `subagent` listing them; dispatch through `execute_all` creates running jobs; manual TUI run: dispatching works, pane appears on tool result.

---

## Phase 4 — TUI integration: live pane + auto-injection (Rust)

Goal: idle agents visible at boot, live status updates, results wake the main agent.

**4a. `src/ext/types.rs` — `ExtensionManager::render_subagent_pane(jobs: &serde_json::Value) -> Option<serde_json::Value>`** (~20 lines): follow `dispatch_simple`/`guard_with_bone` pattern; gate on `engine_ok`; look up `bone._subagents_render`, call with `lua.to_value(jobs)`, convert returned table; warn on Lua error.

**4b. `src/ui/app/mod.rs`** (~60 lines):
- New `App` field `subagent_seen_version: u64`, init `u64::MAX` (forces first-tick render → idle agents appear at boot).
- In `run()` loop after the `event::poll` block (~line 313): `self.tick_subagents(&mut terminal).await?;`
- `tick_subagents`:
  1. Pane refresh: if `registry().version() != subagent_seen_version` → update, `refresh_subagent_pane()`, redraw. (Cheap atomic compare per 50ms tick.)
  2. Auto-injection, only when idle: `active_prompt.is_none() && !streaming && input.buffer.is_empty() && queue.is_empty()`. Then `take_finished_unconsumed()`; if non-empty → `submit_user_turn(text, Some(display), term).await?` + the same queue-drain loop `handle_key` uses (~mod.rs:719-723).
- `refresh_subagent_pane()`: `registry().snapshot()` → `extensions.render_subagent_pane()` → `PanePage::from_json` → `upsert` (or `remove` if content empty).
- Free fn `format_subagent_results(&[Job]) -> (String, String)`:
  - turn text: `"[automated message] Background sub-agent results are ready. Review and continue.\n\n## researcher (job-3) — done\n<truncated result>..."`
  - display text (compact scrollback): `"[subagent results: researcher ✓, coder ✗]"`
  - User-role turn via `submit_user_turn` is correct: lands in transcript/history/DB like normal input and wakes the model.
- `take_finished_unconsumed` bumps version → pane shows "done" on next tick automatically.

**4c. Optional polish — `src/ui/app/stream/mod.rs`**: in `consume_stream`'s spinner-tick arm (~line 429), version check + `refresh_subagent_pane()` so the pane updates while the main agent streams. **Do NOT** add to `wait_for_tool_future_live` (a Lua tool may hold the VM mutex → UI stall). Skip `wait_for_stream` if borrow-splitting complicates it.

**Phase check**: manual TUI run — idle agents visible at boot; dispatch → pane shows running; when done while idle, result auto-injects and the agent responds.

---

## Phase 5 — Tests + docs

**`tests/subagent_test.rs`** (follow `tests/lua_api_test.rs`: temp config dir, `boot_with_tools`, multi-thread runtime):
1. Registration + dynamic description: 2 agents in init.lua → `subagent` tool lists both; no tool when none registered.
2. Spawn lifecycle (no provider → job ends Error): spawn returns `{ok=true, id}`; poll registry until Error; `take_finished_unconsumed` returns it exactly once.
3. Depth guard: `agent_depth=1` → spawn `ok=false`.
4. Render path: `render_subagent_pane(&fake_jobs)` → valid `PanePage::from_json`.
Note: registry is process-global — assert on returned ids, not global counts.

**Docs**: update ctx/Lua API docs (where commit `ab1aa6f` put them) with `ctx.agent.spawn/jobs`, `bone.register_subagent`, auto-injection behavior. Remove or fold in `subagent-spec.md`.

**Final verification**: `cargo test`, `cargo clippy`, manual TUI session with sample `init.lua` registrations.

---

## Edge cases (decided)

- App exit with running jobs: detached tasks dropped — acceptable.
- Esc: cancels main turn only; jobs keep running.
- Job completes mid-stream: injected on first idle tick after the turn.
- User typing when results arrive: injection deferred until input + queue empty (retried each tick).
- Duplicate dispatch to busy agent: rejected in Lua.
- Huge results: truncated to 16k chars per job at injection (full result stays in registry).
- `/tools reload`: VM re-boots, hooks recreated from init.lua; global registry survives.

## Critical files

| Phase | File | Change |
|---|---|---|
| 1 | `src/ext/jobs.rs` | NEW — registry |
| 1 | `src/ext/mod.rs` | +`pub mod jobs;` |
| 1 | `src/ext/ctx.rs` | `spawn`/`jobs` in `add_agent_table()` (line 851) |
| 2 | `src/ext/ops_tools.rs` | `setup_register_subagent` |
| 2 | `src/ext/engine.rs` | wire it (~line 57) |
| 3 | `defaults/lua/tools/subagent.lua` | NEW — tool + pane renderer |
| 4 | `src/ext/types.rs` | `render_subagent_pane` |
| 4 | `src/ui/app/mod.rs` | field + `tick_subagents` + `refresh_subagent_pane` + `format_subagent_results` |
| 4 | `src/ui/app/stream/mod.rs` | (optional) spinner-tick pane refresh |
| 5 | `tests/subagent_test.rs` | NEW |
