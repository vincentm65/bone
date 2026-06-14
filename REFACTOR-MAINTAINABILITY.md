# Maintainability Track — Structural Refactor

Pure maintainability work: no behavior change, real regression risk, no immediate
user payoff. **Only pursue an item if it's actually blocking feature work or
repeatedly costing you time.** Don't start the big god-object splits without a
test safety net (see Preconditions).

> Line numbers from the original audit are stale — re-locate each site before
> editing, and treat the size claims ("893-line", "294-line") as approximate.

---

## Preconditions (do before any large split)
- **Characterization tests** around the turn/stream lifecycle before touching
  #5/#11. Extraction without tests on this path is the highest-risk work here.
- Decide a stopping point per item up front. Half-finished god-object splits are
  worse than the original — commit each extraction so it stands alone.

---

## Highest leverage (start here)

### M1. Dedup before-turn / token-estimation context (was audit #16) — ✅ DONE
**Files:** `src/agent.rs`, `src/ui/app/stream/mod.rs`, `src/ui/app/mod.rs`
Same `CtxConfig` build with `schema_chars / 3.8` magic + `by_provider` query is
triplicated across three files.
**Fix:** One `build_before_turn_config()` + `estimate_prompt_tokens()` helper.
**Why first:** Genuine leverage; unblocks the #5/#11 splits below.

---

## God-object splits (only if blocking)

### M2. `create_ctx_table` god function (was audit #1)
`src/ext/ctx.rs` — builds the entire Lua `ctx` table in one ~900-line fn with
~15 inline closures.
**Fix:** One module per sub-table (`ctx/fs.rs`, `ctx/agent.rs`, …); god fn becomes
a ~20-line orchestrator.

### M3. `add_agent_table` (was audit #2)
`src/ext/ctx.rs` — four huge closures; `run_stream_fn` has three verbatim copies
of the event-drain loop and tripled opts parsing.
**Fix:** Extract `run_stream_fn` to a named async fn; factor the drain loop; build
`AgentOpts` once. Pairs naturally with M2.

### M4. `App` god struct + `handle_key()` (was audit #3)
`src/ui/app/mod.rs` — 40-field struct mixing unrelated concerns; flat ~150-line
match.
**Fix:** Split into `SessionState`/`TurnState`/`PaneState`/`StreamingState`;
extract `handle_pane_key`/`handle_autocomplete_key`/`handle_prompt_key`.

### M5. `submit_user_turn()` (was audit #5)
`src/ui/app/stream/mod.rs` — orchestrates the whole LLM lifecycle in ~210 lines.
**Fix:** Split into `build_turn_context`/`stream_with_retry`/`handle_tool_loop`/
`finalize_turn`. **Depends on M1.** Needs characterization tests first.

### M6. `drain_keys()` duplicates `handle_key()` (was audit #6)
`src/ui/app/stream/mod.rs` — second copy of the key-event state machine, already
diverged (paste-burst redraw suppression only in `handle_key`).
**Fix:** Merge into one `handle_events(timeout)`; share dispatch helpers. Do with
or after M4.

### M7. `run_agent()` monolith (was audit #11)
`src/agent.rs` — 294-line fn, no explicit state machine.
**Fix:** `enum AgentState`; extract `consume_stream()` +
`build_before_turn_context()` (shared with M1) + `handle_token_usage_or_estimate()`.

---

## Provider dedup

### M8. Shared SSE + provider scaffolding (was audit #7)
`src/llm/providers/openai_compat` + `codex.rs` — ~40% shared code hidden via
direct cross-imports (`codex.rs` imports `PartialToolCall`,
`flush_partial_tool_calls` from openai_compat).
**Fix:** Extract `src/llm/providers/shared.rs` (PartialToolCall, flush,
ThinkParser, SSE loop core); shared `ProviderClient` base.

### M9. Provider config heuristics (was audit #22)
`stream_options.include_usage` decided by `base_url.contains(...)`; codex always
prefers `~/.codex/auth.json` over configured key (un-overridable).
**Fix:** `stream_options` as a config flag; config-key-first auth resolution.
Pairs with M8.

---

## Smaller dedup / cleanup (opportunistic)

| ID | Was | Location | Fix |
|----|-----|----------|-----|
| M10 | #13 | `defaults/lua/commands/memory.lua` | Extract `dream_prompt(...)` helper (4× copy-paste) |
| M11 | #17 | `src/agent.rs` `emit_event()` | `From<&AgentEvent>` impls instead of dual 16-arm matches |
| M12 | #18 | `src/agent.rs` + `src/run.rs` | Shared `parse_common_args()` in `src/cli.rs` |
| M13 | #19 | `src/ext/types.rs` `parse_lua_return_action` | `lua.from_value::<Value>()` + serde structs |
| M14 | #20 | all `tools/*.rs` | Derive schema via `schemars`; `impl_tool!` macro |
| M15 | #15 | `src/agent.rs` `SessionWriter` | Hold an open `SessionDb`, not a path (reopens per call) |
| M16 | #14 | `src/tools/edit_file/mod.rs` | `MatchingStrategy` enum; move error fmt to display layer |
| M17 | #23 | `defaults/lua/commands/compact.lua` | Single backward walk; drop redundant sanitization |
| M18 | #10 | `defaults/lua/tools/cron.lua` | Rewrite Python-in-heredoc as pure Lua |

Tier-3 audit items (#24–36) are deferred indefinitely unless touched incidentally.

---

## Explicitly out of scope (redesigns, not refactors)
- **Audit #9** (data-driven command policy / "let YAML drive classification") —
  this is a redesign that will balloon. Treat as its own project, not a refactor.

## Suggested order
1. M1 (leverage / unblocks others)
2. M2 + M3 (ctx.rs — biggest file, enables ext-layer work)
3. M4 + M6 (UI god-objects) — tests first
4. M5 + M7 (turn/agent loops) — depend on M1, tests first
5. M8 + M9 (providers)
6. Smaller items (M10–M18) opportunistically
