# Subagent System Cleanup Plan

Status: **done.** All work items landed (#1, #2, #5, #6, test gaps) plus the
two sign-off forks (#3A delete `parent`, #4a/#4b/#4c opt-in batch). Build clean,
414 tests pass, no warnings. Remaining open item: Tier 3 recursion (its own plan
doc, `subagent_redesign_tier3_plan.md`).

Implements the review verdict — the five-layer design is correct, this pass
removed patchwork without changing UX.

## Verdict (no action — recorded for context)

The subagent system is correctly **not** a single Rust tool. It is a clean
five-layer system:

1. Model-facing Lua tool — `defaults/lua/tools/subagent.lua` (378 lines):
   `dispatch`/`wait`/`cancel`/`status`.
2. Lua primitives — `src/ext/ctx.rs` `add_agent_table` (line 1239):
   `ctx.agent.{run,run_stream,spawn,wait,cancel,jobs}`.
3. Registry — `src/ext/jobs.rs` (444 lines): global `JobRegistry`, concurrency
   caps, cancel flags, result spill/truncate, versioning.
4. Agent loop — `src/runtime/driver.rs` `run_agent` → `Driver::run`, shared by
   headless and TUI. `AgentRunEvent` is a pure type alias to `RuntimeEvent`
   (`agent.rs:467`); `run_agent` is thin setup-then-driver.
5. Live pane — `src/ui/subagent_pane.rs` (206 lines), Rust-native so it stays
   live while a Lua tool blocks the VM.

Boundaries are genuinely good. Build is clean, no warnings. **This plan does
not touch the layering.**

## Status vs. existing redesign docs

- **Tier 1** (concurrency / cancel / parent) — DONE, well-tested in
  `src/ext/jobs_tests.rs`.
- **Tier 2** — HALF-DONE. `build_agent_request` (ctx.rs:1566) extracted
  request-building, but the watchdog+cancel `select!` loop was never unified.
  The three dispatch closures (`run` / `run_stream` / `spawn`) each reimplement
  it. **This is the core of the "patchwork" feeling and the main target below.**
- **Tier 3** (recursion) — NOT DONE. `MAX_AGENT_DEPTH = 1` (ctx.rs:1213), no
  `scope` on `AgentRequest`, driver has no self-injection, auto-injection lives
  only in the TUI (`tick_subagents` app/mod.rs:866, `refresh_subagent_pane`
  app/mod.rs:1025, via `peek_finished_unconsumed` / `mark_consumed`).

---

## Work items

### Core refactor — zero UX change (do all of these)

#### #1 Fix `tools` warning bug (bugfix, ~1 line)

`SPAWN_OPT_KEYS` (ctx.rs:1630-1638) omits `"tools"`. Passing a per-agent
`tools` allowlist to `ctx.agent.spawn` triggers a spurious stderr warning
through `warn_unknown_opts` even though the allowlist is honored.

- Add `"tools"` to `SPAWN_OPT_KEYS`.
- Verify the allowlist still narrows (covered by
  `tool_allowlist_narrows_exposed_tools` at the Rust level).

#### #2 Collapse the three dispatch closures into one helper (main cleanup)

`run` (ctx.rs ~1240-1290), `run_stream` (~1300-1345), and `spawn`
(~1356-1452) each reimplement the same watchdog+cancel pattern:

```rust
tokio::select! {
    result = crate::agent::run_agent(request) => ...,
    _ = await_cancelled(&cancel_watch) => Err("cancelled"),
    _ = inactivity_elapsed(activity, timeout_ms) => Err(inactivity_message(...)),
}
```

Extract a single `build_and_spawn(...)` (working name) that takes the built
`AgentRequest`, the cancel watch, the activity/timeout pair, and the token
callbacks, and returns the outcome. `run` / `run_stream` / `spawn` become thin
callers that differ only in: blocking vs. background, stream-callback wiring,
and result delivery (return value vs. `complete_with_tokens`).

- Expected: ~360 → ~180 lines in the dispatch region. Matches the Tier 2.1 goal.
- Safe to refactor: `run` and `run_stream` have **zero** Lua-level tests (only
  the depth-limit probe touches `ctx.agent.run`, and only `spawn` is exercised
  end-to-end). Collapsing them cannot regress a covered path.
- Keep behavior identical: same error strings, same cancel semantics, same
  token accounting.

#### #5 UI cleanup — single source for `has_running`, dedupe pane removal

`refresh_subagent_pane` (app/mod.rs:1025-1055) acquires the registry lock twice
(`all_jobs()` then `running_ids().is_empty()`) and independently calls
`PanePage::remove` for `PANE_SOURCE` — duplicating the removal already done in
`maybe_refresh_subagent_pane` (app/mod.rs:943-948).

- Derive `has_running` from the single `all_jobs()` snapshot (`jobs.iter().any(
  |j| j.status == Running)`), one lock acquisition.
- Pick **one** pane-removal path. Keep it in `refresh_subagent_pane` (the
  low-level renderer) and let `maybe_refresh_subagent_pane` rely on it; remove
  the duplicate `PanePage::remove` block in `maybe_refresh_subagent_pane`.
  Preserve the `panes_visible = true` unhide-on-start behavior.

**Caveat — do NOT touch:** `shown_tool_rows` (app/mod.rs:117, :230) is alive —
used in 9 sites across `src/ui/app/stream/mod.rs` (:302, :606, :640, :780, :802,
:810, :824) for streaming tool-row dedup. It is not subagent-related and must
not be dropped in this pass.

---

### Tiny observable tweaks — opt-in, need sign-off (flag, don't assume)

These are invisible-to-internal but visible on-screen or to the model. Each is
a one- or two-line change. **Default: leave as-is unless approved.**

#### #4a Status glyph mismatch

- Status bar uses `◐` (app/mod.rs:1098).
- Pane header and rows use `◑` (subagent_pane.rs:54, :166).
- Lua tool status line uses `◑` (subagent.lua:54).

Pick one glyph everywhere. Recommend `◑` (matches the pane, which is the
primary surface). Touches 4 sites.

#### #4b Truncation marker mismatch

- Inline injection: `"\n[... output truncated ...]"` (jobs.rs:418,
  `TRUNCATION_MARKER`).
- Lua tool result: `"\n[... truncated]"` (subagent.lua:156).

These are two different delivery paths but the strings are inconsistent and
both reach the model. Recommend reusing the `TRUNCATION_MARKER` const from Lua
(via `ctx` or a shared constant) so they can't drift.

#### #4c Tool description vs. actual concurrency

The `subagent.lua` tool description states agents run "one job at a time", but
`ctx.agent.spawn` honors `max_concurrency` (default 1, but settable per
template). Update the description text to reflect `max_concurrency` so the
model isn't misled.

---

### Helper de-dup — invisible (safe to do with #2)

#### #6 Consolidate `current_unix_seconds`

Duplicated: `src/ext/jobs.rs:392` and `src/ui/subagent_pane.rs:205`. Move to one
location (e.g. a small `src/ext/util.rs` or re-export from `jobs.rs`) and
import in both. No behavior change.

---

## Decision fork — NOT a refactor (user must choose direction)

### #3 Tier 3 recursion vs. delete the dead `parent` field

`Job.parent: Option<String>` (jobs.rs:55) and `NewJob.parent` (jobs.rs:71) are
prep for Tier 3 recursion. Today they are **set to `None` at the only write
site** (ctx.rs:1401) and **never read** anywhere in non-test code (the
`.parent` grep hits are all unrelated `Path::parent()`). The field is dead
weight until Tier 3 lands.

Two mutually exclusive directions:

- **(A) Delete `parent`.** Remove the field from `Job` and `NewJob`, drop the
  `None` assignment at ctx.rs:1401 and the doc comments. ~6 lines removed. No
  behavior change. Keeps the code honest about what's implemented.
- **(B) Implement Tier 3 recursion.** Large: requires `scope`-keyed
  `parent`/`scope` plumbing on `AgentRequest`, making the `Driver` loop
  injection-aware (currently injection is TUI-only via
  `peek_finished_unconsumed`/`mark_consumed`), and raising
  `MAX_AGENT_DEPTH`. Not in scope for a cleanup pass; would be its own plan.

**Recommendation: (A) now.** Tier 3 has its own plan doc
(`subagent_redesign_tier3_plan.md`); deleting the dead field now does not
block re-adding it when Tier 3 is actually built.

---

## Test gaps to fill (alongside #2)

- **Cancel end-to-end:** the `cancel` action is never tested through the Lua
  tool. Add a Lua-level test that spawns a job and cancels it via
  `ctx.agent.cancel` (or the `subagent` tool's `cancel` action), asserting the
  job reaches `Cancelled` and `complete_with_tokens` fires with the cancel
  error.
- **`tools` allowlist full path:** currently only tested at the Rust-plumbing
  level (`tool_allowlist_narrows_exposed_tools`). Add a Lua-level test that
  passes `tools={...}` to `ctx.agent.spawn` and confirms the spawned agent's
  exposed tools are narrowed. (Also serves as the regression test for #1.)
- **`run` / `run_stream` Lua coverage:** optional. They work and have zero
  Lua tests; adding a minimal happy-path test for each would protect the
  #2 collapse. Lower priority than the two above.

Note on the existing suite: `spawn_lifecycle_no_provider`
(subagent_test.rs:219) uses a 30s polling sleep — a CI-flake pattern. The
`dispatch{wait:true}` test avoids it by blocking. Consider converting the
polling test to the blocking pattern while in the area, but it is not required
for this pass.

---

## Execution order

1. **#1** (1 line, unblocks #2's regression test).
2. **#2** (core refactor) + **#6** (consolidate `current_unix_seconds`, natural
   to do while in `ctx.rs`/`jobs.rs`).
3. **#5** (UI cleanup).
4. Fill the cancel + tools-allowlist test gaps.
5. **#3(A)** delete `parent` (pending sign-off on direction A).
6. **#4a / #4b / #4c** (pending sign-off — present as a single batch).

Each step compiles + `cargo test` before moving on. No UX change ships without
explicit approval (steps 5 direction, step 6 batch).
