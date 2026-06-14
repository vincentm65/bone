# Refactor Audit — Messiest Code in `bone`

Synthesized from parallel sub-agent analysis of all 5 layers
(UI, Ext/Lua, Agent/Core, Tools, LLM+Lua). 43 hotspots identified;
deduplicated and ranked by maintainability impact.

---

## TIER 1 — HIGH (do these first)

### 1. `src/ext/ctx.rs:98–990` — `create_ctx_table` 893-line god function
Builds the ENTIRE Lua `ctx` table (fs/log/ui/shell/agent/tools/config/session) in one function with ~15 inline closures of 30–80 lines each. Inconsistent return shapes, duplicated error-table construction. Directly violates "Rust owns primitives."
**Fix:** Split into one module per sub-table (`ctx/fs.rs`, `ctx/agent.rs`, …); god fn becomes a ~20-line orchestrator.

### 2. `src/ext/ctx.rs:991–1357` — `add_agent_table` 367-line monster
Four huge closures (`run`/`run_stream`/`spawn`/`wait`). `run_stream_fn` alone ~200 lines with **three verbatim copies** of the event-drain loop and tripled `parse_agent_opts` parsing.
**Fix:** Extract `run_stream_fn` to a named async fn; factor drain loop; build `AgentOpts` once.

### 3. `src/ui/app/mod.rs` — `App` 40-field god struct + `handle_key()` (670–820)
Mixes LLM state, rendering, session DB, Lua, keymaps, subagents, autocomplete, approval, timers, paste detection. `handle_key()` is a ~150-line flat match maze of early returns.
**Fix:** Split into `SessionState`/`TurnState`/`PaneState`/`StreamingState`; extract `handle_pane_key`/`handle_autocomplete_key`/`handle_prompt_key`.

### 4. `src/session_db.rs:105–152` — destructive schema migrations
On any `user_version` mismatch it **DROPs every table and recreates from scratch** — every schema bump destroys all user history + usage. No ALTER / data-preserving path.
**Fix:** Versioned `ALTER TABLE` migration chain.

### 5. `src/ui/app/stream/mod.rs` — `submit_user_turn()` (100–310, ~210 lines)
Orchestrates the entire LLM lifecycle: before_turn hook, history build, nested `loop{ loop{ } }` for retries + tool chains, display rows, DB writes, queue drain. Inline `ctx_cfg` build **duplicated verbatim** in `run_lua_command()`.
**Fix:** Split into `build_turn_context`/`stream_with_retry`/`handle_tool_loop`/`finalize_turn`; share `build_before_turn_config()`.

### 6. `src/ui/app/stream/mod.rs:1050–1180` — `drain_keys()` duplicates `handle_key()`
Second independent copy of the entire key-event state machine. Already diverged (paste-burst redraw suppression exists only in `handle_key`).
**Fix:** Merge into one `handle_events(timeout)`; share dispatch helpers.

### 7. `src/llm/providers/openai_compat` + `codex.rs` — duplicated SSE + provider scaffolding
~50-line identical streaming loop boilerplate in both `chat_stream()`, ~18-line HTTP-error block duplicated, and `codex.rs:112` **imports types directly from openai_compat** (`PartialToolCall`, `flush_partial_tool_calls`). Identical `ProviderEntry` structs + `from_entry()` constructors.
**Fix:** Extract shared `src/llm/providers/shared.rs` (PartialToolCall, flush, ThinkParser, SSE loop core); shared `ProviderClient` base struct.

### 8. `src/ui/render/bottom_pane.rs:250–530` — `draw_bottom_pane_with_tick()` 280-line megafunction
Manual `y += 1` coordinate bookkeeping across separator/tab/prompt/input/autocomplete/page/status, ~80-line nested prompt-layout block handling 4 layout modes.
**Fix:** Extract `draw_separator`/`draw_prompt_region`/`draw_input_field`/`draw_autocomplete`/`draw_page_region`/`draw_status_bar`.

### 9. `src/tools/command_policy/mod.rs:118–273` — `classify_segment` 155-line if/else dump
~30 special-cased heuristic rules, `systemctl` checked 3×, mixes string arrays + positional checks + substring + redirection parsing + pipe detection. The YAML policy file is nearly redundant because everything is hardcoded here.
**Fix:** Data-driven rule list / predicate pipeline; let YAML actually drive classification.

### 10. `defaults/lua/tools/cron.lua` — 222-line embedded Python via brittle shell escaping
Entire impl is a Python heredoc run through `ctx.shell()` with **manual `gsub('"','\\"')` escaping** of params via env vars. Untyped, unlintable, hidden from tooling, depends on `uv run` at runtime.
**Fix:** Rewrite in pure Lua using `ctx.shell()` for `crontab`; pass params as JSON stdin or argv.

---

## TIER 2 — MEDIUM

### 11. `src/agent.rs:237–530` — `run_agent()` 294-line monolith
No explicit state machine; stream consumption mixes text/reasoning/tool-calls/usage/errors in one `while let`; inline 45-line before-turn block at 282–327; fallback token estimation duplicates session recording.
**Fix:** `enum AgentState`; extract `consume_stream()` + `build_before_turn_context()` + `handle_token_usage_or_estimate()`.

### 12. `src/session_db.rs:377–600` — SQL `format!()` + 14-query snapshot
`usage_by_model_since` / `usage_by_hour_since` build SQL with `format!()` on a raw `date_filter` (injection-by-convention). `usage_stats_snapshot()` fires **14 separate queries**. Recursive-CTE bucket query duplicated 3× (day/week/month).
**Fix:** Parameterized queries; merge snapshot into ~3 wider queries; extract CTE helper.

### 13. `defaults/lua/commands/memory.lua:18–239` — 4× copy-paste of ~40-line prompt block
Same "Your task / Rules" template repeated verbatim in 4 branches (~160 of 250 lines are pure duplication).
**Fix:** Extract `dream_prompt(preamble, count, next_run, conv_blocks)` helper.

### 14. `src/tools/edit_file/mod.rs:339–570` — 3-tier fuzzy matching over-engineering
Exact→normalized→Levenshtein pipeline with magic thresholds (`0.92`, `0.08`, `30`), sort+dedup, plus 40-line error-formatting functions mixed into the matching algorithm.
**Fix:** `MatchingStrategy` enum; move error formatting to display layer; document or drop Levenshtein.

### 15. `src/agent.rs:13–78` — `SessionWriter` opens a new SQLite connection every call
`append_message`/`record_usage`/`end` each re-open the DB; 10–20 opens/PRAGMAs per turn; conversation-create opens a 3rd time.
**Fix:** Hold an open `SessionDb`, not a path.

### 16. Cross-cutting: duplicated `before_turn` / token-estimation context
`agent.rs:282–327`, `ui/app/stream/mod.rs:130–170`, `ui/app/mod.rs:950–1010` all build `CtxConfig` with the same `schema_chars / 3.8` magic and `by_provider` query.
**Fix:** One `build_before_turn_config()` + `estimate_prompt_tokens()` helper.

### 17. `src/agent.rs:100–155` — `emit_event()` converts 8-variant enum twice
Builds `AgentRunEvent` (16 arms) AND `serde_json::Value` (16 arms) inline.
**Fix:** `From<&AgentEvent> for AgentRunEvent` + `From for Value`.

### 18. `src/agent.rs:870` + `src/run.rs:5–70` — duplicated CLI arg parsing
Near-verbatim `parse_agent_args` / `parse_run_args`.
**Fix:** Shared `src/cli.rs` `parse_common_args()` builder.

### 19. `src/ext/types.rs:180–250` — `parse_lua_return_action` manual table walking
70 lines of `.get().ok().flatten()` chains, silently `continue`s on malformed entries, duplicates `LuaSerdeExt`.
**Fix:** `lua.from_value::<serde_json::Value>()` + serde structs.

### 20. All `tools/*.rs` — boilerplate duplication
Each tool hand-writes a `json!()` schema that mirrors its `Args` struct verbatim (edit_file: 77-line schema vs 17-line struct), plus `serde_json::from_value().map_err()` + `#[async_trait]` wrapper.
**Fix:** Derive schema via `schemars`; `impl_tool!` macro for boilerplate.

### 21. `src/ext/ops_plugins.rs` — entire file is one fn with 5 inline closures
`list` uses `unwrap_or_else(|e| panic!(...))` → crashes Lua VM on I/O error; `install` mixes `block_in_place`+`block_on`+`Command`.
**Fix:** Named async fns in a `plugins/` submodule; real error propagation.

### 22. `src/llm/providers` — hardcoded URL heuristics + auth precedence
openai_compat uses `base_url.contains("api.openai.com"|"127.0.0.1"|"localhost")` to decide `stream_options.include_usage`. codex **always** prefers `~/.codex/auth.json` over configured key (un-overridable).
**Fix:** `stream_options` as config flag; config-key-first auth resolution.

### 23. `defaults/lua/commands/compact.lua` — 6 passes where 2 suffice
Backward + forward walk + `sanitize_tool_chains` (3 more passes); sanitization is redundant on synthetic compaction output.
**Fix:** Single backward walk; drop sanitization for compact output.

---

## TIER 3 — LOW (cleanup / nice-to-have)

| # | Location | Issue |
|---|---|---|
| 24 | `src/ext/ctx.rs:178–290` | 112-line manual process/thread/channel orchestration inside a Lua closure; reader thread leaks on panic |
| 25 | `src/ext/ctx.rs:385–415` | `static INTERACT_MUTEX` declared inside closure → global deadlock if user never responds |
| 26 | `src/ext/lua_tool.rs:150–220` | Manual `drop(lua)` guard with no mechanical enforcement of the reentrancy invariant |
| 27 | `src/tools/command_policy:60–109` | `peel_shell_args` fragile string-peeling; `"commandwithargs"` likely dead |
| 28 | `src/tools/shell.rs:48–106` | Triple-nested anonymous async blocks in `run_script` |
| 29 | `src/tools/edit_file:226–296` | `parse_operation` redundant `kinds` counter + silent LLM-tolerant `text` fallback hack |
| 30 | `src/ui/app/stream/mod.rs:900–990` | `wait_for_stream` duplicates `timer_elapsed()` + `renderer.tick_spinner()` |
| 31 | `src/ui/stats.rs` (776L) | Standalone full-screen TUI app duplicating `RawModeGuard`/`BoneBackend`/event loop/keymap |
| 32 | `src/ext/ops_*.rs` + `engine.rs` | Duplicate registration boilerplate + repeated stub-closure creation |
| 33 | `src/ui/render/mod.rs:140–210` | Hand-rolled markdown-streaming state machine (pulldown_cmark already in tree) |
| 34 | `src/main.rs:210–330` | Flat if/else of 4 entry points + `ensure_deps` infra in binary |
| 35 | `src/config/providers_config.rs:68–109` | 3 near-identical `string_or_default*` deserializer helpers |
| 36 | Magic numbers scattered | `3.8`, `0.92`, `0.08`, `30`, `90`, `500`, `60_000` across many files |

---

## Cross-cutting themes (the real story)

1. **God-objects everywhere:** `App` (40 fields), `create_ctx_table` (893L), `run_agent` (294L), `draw_bottom_pane_with_tick` (280L), `classify_segment` (155L). The dominant anti-pattern.
2. **Triplicated before-turn/token-estimation logic** spanning 3 files — the single highest-leverage extraction (#16).
3. **Duplicated providers:** openai_compat + codex share ~40% of their code but hide it via direct cross-imports.
4. **SQL:** destructive migrations (#4) + injection-by-convention `format!()` (#12) are the only true *correctness* risks (vs. maintainability).
5. **Lua policy layer mostly clean** except cron.lua (Python-in-heredoc) and memory.lua (4× duplication).

## Recommended order
1. **#4 destructive migrations** (data-loss risk — fix now)
2. **#16 before_turn context dedup** (touches 3 files, unblocks #5/#11)
3. **#1 + #2 ctx.rs split** (biggest single file, enables all ext-layer work)
4. **#3 + #6 App/handle_key split** (UI layer god-objects)
5. **#7 + #22 providers shared module** (LLM layer)
6. Then Tier 2 items in dependency order.

## Clean — leave alone
`chat.rs`, `lib.rs`, `read_file.rs`, `write_file.rs`, `write_atomic.rs`, `state_map.rs`, `tools/types.rs`, `ext/snapshots.rs`, `ext/jobs.rs`, `ext/engine.rs`, `llm/token_tracker.rs`, `llm/prompts.rs`, `llm/provider.rs`, `ask_user.lua`, `usage.lua`, `customize.lua`.
