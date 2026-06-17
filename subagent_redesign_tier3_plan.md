# Subagent Redesign — Tier 3 (Recursion)

Status: planned. Depends on Tier 1+2 being merged first.

## Problem

`MAX_AGENT_DEPTH = 1` (ctx.rs:1162) hard-caps subagent recursion at one
level. A sub-agent can never delegate, so there is no manager -> worker
orchestration, no fan-out/fan-in trees. The justification is that nested
job results would inject into the wrong conversation (depth 0), because
the auto-injection path is hardwired to the top-level TUI app.

This tier removes that wall, replacing it with a configurable depth
budget and scope-keyed result delivery.

## Goal

Allow sub-agents to spawn their own sub-agents up to a configurable
`max_depth`, with each job's results scoped to its parent's run (not the
global depth-0 conversation). The depth budget replaces the wall.

## Locked decisions

- **Depth model: budget, not cycle-guard.** A configurable `max_depth`
  (default 3), decremented per level. Hard reject at the limit with a
  clear error. Predictable, cheap, bounds token cost. No cycle detection
  (would require result-content hashing; not worth it).
- **Scope = parent run.** Each job records its parent's identity
  (conversation id + agent run/turn key). Auto-injection targets the
  parent scope. A depth-1 agent's depth-2 children inject into the
  depth-1 run, not depth 0.
- **Structured output: deferred.** Results stay free text + optional
  `result_file`. Recursion does not depend on this; decide separately
  after using parallel spawn (Tier 1+2).

## Why this is the expensive tier

Today auto-injection lives in exactly one place: `tick_subagents` in the
TUI app (`src/ui/app/mod.rs:786`), hardwired to the depth-0
conversation. A depth-1 subagent has no TUI — it runs through
`Driver::run`. So the **Driver loop itself must become injection-aware**:
its termination condition changes from "end when no tool calls" to "end
when no tool calls AND no pending children scoped to this run." That is
the one spot where a mistake makes agents hang or loop.

Everything else in this tier is small once scope-injection works.

## Work

### 3.1 Scope-keyed injection (jobs.rs, ~60 lines)

The `parent` field added in Tier 1 (1.3) becomes the scope key.

- `peek_finished_unconsumed_for_scope(scope)` — filter by parent scope,
  not global.
- `mark_consumed(ids)` — unchanged.
- `wait_for(ids, ...)` — unchanged (already id-keyed).
- The global `peek_finished_unconsumed()` stays for the TUI pane (which
  wants to see everything across all scopes) but is no longer the
  injection source.

### 3.2 Thread scope through spawn (ctx.rs, ~20 lines)

- The spawn primitive (built in Tier 2) reads the current scope from
  `ctx` and passes it to `create(..., parent = scope)`.
- `AgentRequest` gains a `scope: Option<String>` so the Driver knows
  which scope it owns.

### 3.3 Driver-side self-injection (driver.rs, ~40 lines) — THE HARD PART

`Driver::run` currently ends the turn when `tool_calls.is_empty()` (the
model produced a final message). Change to:

1. When `tool_calls.is_empty()`, check for finished child jobs scoped to
   this run (`peek_finished_unconsumed_for_scope(self.scope)`).
2. If there are finished children, inject their results as a synthetic
   user/tool message and continue the loop (the model gets another turn
   to use the results).
3. If there are no finished children but some are still running, block
   (wait) until at least one finishes, then inject.
4. Only end the turn when there are no tool calls AND no children at all
   (finished or pending).

This mirrors what `tick_subagents` does, but inside the loop instead of
in the TUI tick. The headless path gets recursion for free because it
goes through `Driver::run`.

Risk: the loop's termination condition. Must ensure that a child job
that errors or is cancelled is still "finished" for injection purposes
(its result is the error string) so the loop doesn't hang waiting on a
dead child.

### 3.4 Remove the depth wall -> budget (ctx.rs, ~20 lines)

- Replace `const MAX_AGENT_DEPTH: usize = 1` with a configurable budget.
- Read `max_depth` from a config field (default 3) — put on
  `AgentRequest` or read from config in `boot_with_tools`.
- The depth check in `run_fn` / `run_stream_fn` / `spawn_fn` becomes
  `if agent_depth >= max_depth { reject }`.
- The Lua `subagent.lua` early exit `if bone.agent_depth > 0 then return
  end` is removed — the tool is now registered at all depths < max_depth.
- Sub-agents CAN spawn (Tier 1 lifted the per-template one-job rule);
  the depth check is the only remaining gate.

### 3.5 Headless parity

`run_agent` / headless runs go through `Driver::run`, so once 3.3 lands
they get self-injection automatically. No separate headless work.

### 3.6 Quit guard (unchanged)

The quit guard already warns when background jobs are running. With
recursion, child jobs are owned by their parent run; killing the process
still terminates everything. No change needed.

## Scope limits (explicitly NOT in this tier)

- **Unlimited depth + cycle detection** — explicitly rejected (budget
  model instead).
- **Per-agent token/turn budgets** — cancel covers runaways; budgets
  are a separate enhancement.
- **Structured output schema** — deferred.

## Tests to add/extend

- `src/ext/jobs_tests.rs` — scope-keyed peek returns only matching
  parent; global peek still sees all.
- `tests/subagent_test.rs` — depth-1 agent can spawn depth-2 children;
  depth-2 results inject into the depth-1 run (not depth 0);
  `max_depth` rejection at the limit.
- New test: Driver self-injection — a mock depth-1 run with a pending
  child does NOT terminate until the child finishes and its result is
  injected. This is the regression guard for the loop-hang risk.

## Sizing

| Piece | Files | Lines touched | Risk |
|---|---|---|---|
| 3.1 scope-keyed peek | jobs.rs | ~60 | medium |
| 3.2 thread scope | ctx.rs, agent.rs | ~20 | low |
| 3.3 driver self-inject | driver.rs | ~40 | **high** |
| 3.4 depth wall -> budget | ctx.rs, subagent.lua | ~20 | low |
| 3.5 headless parity | (subsumed by 3.3) | 0 | — |
| tests | jobs_tests.rs, subagent_test.rs, new | ~200 | — |

Total: ~3-5 days. The driver-loop change (3.3) is ~40 lines but is the
single highest-risk edit in the whole redesign. Land it behind the
mock-driver self-injection test before anything else in this tier.

## Sequencing within the tier

1. 3.1 + 3.2 (scope plumbing, no behavior change) — land together.
2. 3.3 (driver self-inject) + its regression test — land alone, verify.
3. 3.4 (remove the wall) — flip on once 3.3 is green.

## Honest note on whether to do this at all

Tier 1+2 (parallel same-template spawning + manual fan-out/fan-in via
`dispatch` + `wait`) likely covers ~90% of what recursion would give
you, at 1/3 the risk. The case for Tier 3 is specifically: tasks where a
sub-agent's task is itself decomposable and the parent agent can't
predict the shape of the decomposition up front. If, after using Tier
1+2, you don't hit that, skip this tier.
