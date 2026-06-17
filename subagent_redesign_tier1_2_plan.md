# Subagent Redesign — Tier 1 + 2 (Foundation)

Status: planned. Prerequisite for Tier 3 (recursion).

## Problem

The current subagent system conflates "agent definition" (a template)
with "running job" (an instance), and routes all job results into a
single global scope (depth 0). This causes:

- **One-job-per-agent rule** — the registry rejects a spawn if the named
  agent already has a running job. You cannot run 3 parallel "researcher"
  tasks; you must register `researcher-1`, `-2`, `-3`.
- **No per-job cancellation** — a runaway background job can only be
  stopped by quitting the app or waiting out the inactivity timeout.
- **Three duplicated dispatch closures** — `run` / `run_stream` / `spawn`
  in `ctx.rs` (`add_agent_table`) each re-implement the same
  `parse_agent_opts` -> build `AgentRequest` -> `inactivity_elapsed`
  pattern. ~360 lines that are ~80% identical.
- **No per-agent tool scoping** — every subagent inherits all tools; the
  only lever is `approval` (which gates writes, not availability).

## Goal

Land concurrency, per-job cancellation, and per-agent tool allowlists
while collapsing the three dispatch closures into one spawn primitive
plus a wait primitive. UX is unchanged for users who don't opt into the
new optional fields.

## Locked decisions

- **Concurrency model**: per-template `max_concurrency` field (default
  1, preserving current behavior). The registry counts running jobs per
  template name and rejects spawns over the cap.
- **Dispatch primitives**: `spawn` (always async, returns a handle) and
  `wait` (the only blocking op). `run` becomes `spawn` + `wait` in one
  call (convenience). `run_stream` becomes `spawn` + a streaming
  subscription. The model-facing `subagent` tool keeps `dispatch` /
  `wait` / `status` actions; `dispatch{wait=true}` is spawn+wait.
- **Pane format**: keep per-job lines, add a template label prefix so
  multi-job templates are legible. Do NOT collapse to a single count
  row — the task previews are the useful part.
- **Tool allowlist**: optional `tools = {...}` field on the template.
  Sources the `enabled` list passed to `boot_with_tools` (which already
  filters by an enabled list). Omit to inherit all tools (current
  behavior).

## Tier 1 — registry additions (isolated, ~½–1 day)

All in `src/ext/jobs.rs`. Extend `jobs_tests.rs`.

### 1.1 Concurrency cap

`JobRegistry::create` currently rejects when an agent has any running
job. Change to count running jobs per `agent` name and reject only when
the count >= the template's `max_concurrency`.

- `create(agent, task, max_concurrency)` — new param.
- Cap defaults to 1 (callers that omit it get current behavior).
- Error message names the cap: `agent 'X' is at its concurrency cap (N)`.

### 1.2 Per-job cancellation

Add a cancel flag per job, settable by id.

- `Job.cancel_flag: Option<Arc<AtomicBool>>` (populated on `create`).
- `JobRegistry::cancel(id)` — sets the flag.
- The spawn closure already has an inactivity watchdog; add a sibling
  arm that respects the job's own cancel flag (same pattern as the
  parent cancel flag in `run`/`spawn`).
- Expose `ctx.agent.cancel(id)` in the Lua `agent` table.
- Add a `cancel` action to the `subagent` tool: `cancel` with `ids[]`.

### 1.3 parent field on Job (prep for Tier 3, free now)

- `Job.parent: Option<String>` — the spawning scope key.
- `create(agent, task, max_concurrency, parent)` — parent optional.
- Depth-0 spawns pass `None` for now; Tier 3 fills it in.
- No behavior change yet; just the field + plumbing.

## Tier 2 — collapse closures + tool allowlist (1–2 days)

`src/ext/ctx.rs` `add_agent_table` (~362 lines) + `src/ext/mod.rs`
`boot_with_tools` + `defaults/lua/tools/subagent.lua`.

### 2.1 One spawn primitive

Extract a shared `build_and_spawn` helper that:
1. Parses opts once (`parse_agent_opts`).
2. Reads template fields (concurrency cap, tool allowlist, system_prompt).
3. Constructs the `AgentRequest`.
4. Spawns (async, detached) with the inactivity watchdog + job cancel
   flag wired into the `tokio::select!`.
5. Returns the job id / handle.

`run_fn` = `build_and_spawn` then `block_on(wait(id))`.
`run_stream_fn` = `build_and_spawn` then forward events to callbacks.
`spawn_fn` = `build_and_spawn` then return id.

Target: ~360 lines -> ~180 lines. Net negative.

### 2.2 Per-agent tool allowlist

`boot_with_tools` already accepts an `enabled: &[String]` list and
filters the registry. Thread the template's `tools` field through:
- If the template has `tools = {...}`, pass that list as `enabled`.
- Otherwise pass all tool names (current behavior).
- `MAX_TOOL_CALL_DEPTH` already guards recursion depth at the tool-call
  level; no change needed.

### 2.3 subagent.lua updates (minimal, ~20 lines)

- Read `max_concurrency` and `tools` from the agent definition and pass
  to `ctx.agent.spawn` opts.
- Add `cancel` action: `{ action: "cancel", ids: [...] }`.
- Trim the "Rules" block in `build_description` — the wait/no-wait
  dance is simpler now; keep the batching rule and the "don't duplicate
  delegated work" rule, drop the prescription about ending your turn.

### 2.4 Pane renderer (subagent_pane.rs, ~15 lines)

- Per-job lines stay, prefixed with the template name.
- When a template has multiple running jobs, show a template header row
  then indented job rows. Example:
  ```
   ◑ researcher (2)
      running search the web (12s) 120/80 in/out
      running summarize api (4s) 40/20 in/out
   ○ coder
      idle (200/150 in/out)
  ```

## Scope limits (explicitly NOT in this tier)

- **Recursion** — `MAX_AGENT_DEPTH` stays 1. Tier 3.
- **Structured output schema** — results stay free text + optional
  `result_file`. Deferred (decide after using parallel spawn).
- **Token/turn budgets** — not added here; cancellation covers the
  runaway case.

## Tests to extend

- `src/ext/jobs_tests.rs` — concurrency cap (accept up to N, reject N+1),
  cancel(id) sets flag and completes the job, parent field recorded.
- `tests/subagent_test.rs` (498 lines) — registration of `tools`/`
  max_concurrency` fields, cancel action, dispatch of >1 job to one
  template.
- `src/ext/ctx_tests.rs` — the collapsed spawn helper produces the same
  `AgentRequest` fields as today for the default case.

## Sizing

| Piece | Files | Lines touched | Risk |
|---|---|---|---|
| 1.1 concurrency cap | jobs.rs | ~20 | none |
| 1.2 cancel | jobs.rs, ctx.rs | ~40 | none |
| 1.3 parent field | jobs.rs | ~10 | none |
| 2.1 collapse closures | ctx.rs | 360 -> ~180 (net -180) | medium |
| 2.2 tool allowlist | mod.rs, ctx.rs | ~30 | low |
| 2.3 subagent.lua | subagent.lua | ~20 | low |
| 2.4 pane | subagent_pane.rs | ~15 | low |

Total: ~2-3 days. The collapse (2.1) is the only medium-risk piece; it
must land before Tier 3 because adding scope-injection to three
duplicated closures is a nightmare.
