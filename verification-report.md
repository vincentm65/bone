# Verification Report — Over-Engineering Audit

- **Date:** 2026-09-01
- **Subject:** [`OVER_ENGINEERING_AUDIT.md`](./OVER_ENGINEERING_AUDIT.md) (41 findings, 5 groups)
- **Result:** **27 VERIFIED · 13 PARTIALLY VERIFIED · 1 REFUTED**

## Method

Five **fresh** reviewer sub-agents ran in parallel — one per audit group (A: `ext/ctx.rs`, B: `rpc/`, C: `session_db.rs` + `runtime/driver.rs`, D: `config/`, E: misc large files). Each agent was instructed to **distrust the audit document**: treat every claim as unproven, re-run the call-site greps itself, check the cited line references against the actual source, and actively hunt for counter-evidence (hidden consumers in `core/tests`, `core/defaults`, `tui`, `webui`, `protocol`, `npm`, and the Lua fixture files). All checks were static (grep/read); no code was modified and no builds or tests were run. The "verified clean" list was spot-checked the same way.

A finding is **VERIFIED** when its core claim and proposed fix hold (minor line-reference or count corrections noted). **PARTIALLY VERIFIED** when the core claim holds but a cited detail, count, or the stated fix was materially wrong and needed correction. **REFUTED** when the claim is false.

## Group A — `ext/ctx.rs` (9 findings: 9 VERIFIED)

| ID | Verdict | Notes / corrections folded in |
|---|---|---|
| CTX-1 | VERIFIED | PNG codec is test-only; zero non-test hits. Test range is `ctx_tests.rs:1105-~1795` (not 1454); the two box resamplers are ~60 lines each (~120 combined), not 120 each. |
| CTX-2 | VERIFIED | `ctx.session` table has no consumer; superseded by `lua/lib/history.lua`. |
| CTX-3 | VERIFIED | `turn_nudge` field/param is dead plumbing; live path is the shared `Arc<Mutex<Option<String>>>`. |
| CTX-4 | VERIFIED | Single call site per `pub(crate)` item; triple-duplicated token estimate confirmed. |
| CTX-5 | VERIFIED | No consumer of `ctx.settings.get`. Fix must also update `api_tests.rs:430`, which asserts on the `_get_extension` error text. |
| CTX-6 | VERIFIED | Pure forwarder. 3 test call sites invoke the wrapper (`ctx_tests.rs:218,295,321`) — retarget them in the fix. |
| CTX-7 | VERIFIED | Two equivalent cancel-polling loops. |
| CTX-8 | VERIFIED | Prefix pre-check is redundant behind `Statement::readonly()`. |
| CTX-9 | VERIFIED | All five sub-items hold, with corrections: `execution.subtable` test asserts at `ctx_tests.rs:1918-1919` (not 1922-1923) and there is a second test-only consumer, fixture `core/tests/fixtures/task_list.lua:323`; `UsageProviderContext` re-export is also used by `ctx_tests.rs:402,438` via `use super::*`; `build_before_turn_config` lives at `ctx.rs:4117` (not types.rs). |

## Group B — `rpc/mod.rs` + `codec.rs` (6 findings: 4 VERIFIED, 2 PARTIAL)

| ID | Verdict | Notes / corrections folded in |
|---|---|---|
| RPC-1 | VERIFIED | `SetSetting` has no sender anywhere; the webui test asserts its absence. |
| RPC-2 | **PARTIAL** | The duplication is real but narrower than claimed: only 5 of the 7 listed arms are literally in all three pumps — `status_rx`/`live_rx` and `ApprovalReply`/`KeyReply` arms exist only in the hook/command pumps (`run_turn` uses `background_events_rx`/`conn.next_event`/`conn.send`). The post-completion cleanup sequence is repeated 5 times (2 in `run_managed_hook`, 3 in `run_interactive_command`), and the exact ordering is verbatim only in the cancel/none arms. Consolidation still worthwhile. |
| RPC-3 | VERIFIED | All `Into<HubPublisher>` call sites already pass a `HubPublisher`; the `From<Hub>` impl is unused. |
| RPC-4 | VERIFIED | Byte-budgeter re-serializes per iteration as described. |
| RPC-5 | VERIFIED | Double encoding of recoverability confirmed. |
| RPC-6 | **PARTIAL** | `SessionRequest` (verified) and `finish_subagent_change` (verified; it takes **3** parameters, not 2) hold. The `attach_with_initial` sub-claim is **retracted**: it has 3 call sites (`:535, :543, :556`), and the inner `Err(String)` path is load-bearing at 2 of them (converts to `ConversationLoadFailed`/`Status`, connection continues) — collapsing the double `Result` is **not** behavior-preserving. |

## Group C — `session_db.rs` + `runtime/driver.rs` (7 findings: 4 VERIFIED, 2 PARTIAL, 1 split)

| ID | Verdict | Notes / corrections folded in |
|---|---|---|
| DB-1 | VERIFIED | FTS5 table written per message, never queried in production; orphaned doc comment confirms the deleted search function. |
| DB-2 | VERIFIED | `append_turn` wrapper is test-only; production always calls `append_turn_with_checkpoint`. |
| DB-3 | **PARTIAL** | Production-dead, but **not zero-callers**: two test callers exist (`session_db_tests.rs:1367,1393`). Delete method + update tests in the same commit. |
| DB-4 / SINK-1 | **PARTIAL** | The 9-arg `append_message` path is dead in production (Driver exclusively uses `append_chat_message`; the malformed-JSON fallback state cannot occur). Correction: the repo has **six** `SessionSink` impls, not three — the 3 production ones plus 3 test sinks (`agent_tests.rs:41`, `session_sink_test.rs:43`, `driver_turn_test.rs:1034`) that exercise the trait-default projection, so the fix is larger than the ~130-line estimate. |
| DRV-1 | VERIFIED | Verbatim duplication of private-usage recording. |
| DRV-2 | **PARTIAL** | The inline `before_turn` pipeline is confirmed. Corrections: `DriverHookState` has **14** fields (not 15) with **8** literal sites (not 9), and `before_turn` uses none of the state's fields (it inlines the whole pipeline). |
| DRV-3 | VERIFIED | All three sub-items hold, with corrections: `restore_ephemeral_image_relays` has **2** call sites (`:963, :983`), not 3; `max_message_seq` is also called at `rpc/mod.rs:1971`, which reinforces (not weakens) the stale-`allow(dead_code)` claim. |

## Group D — `config/settings.rs` + `config/store.rs` (4 findings: 3 VERIFIED, 1 PARTIAL)

| ID | Verdict | Notes / corrections folded in |
|---|---|---|
| CFG-1 | VERIFIED | Entire legacy canonical-key routing block dead/test-only as itemized. |
| CFG-2 | VERIFIED | Both theme representations deserialized and applied, flat overwriting nested. Note added: no bundled theme exists in the repo (`config.yaml` has no `theme` key) — the risk is limited to user-installed themes. |
| CFG-3 | VERIFIED | `into_resolved` has zero callers. |
| CFG-4 | **PARTIAL** | Dual ownership is **wider** than reported: beyond `subagents`/`extensions`, `Inner.disabled_tools`/`disabled_commands` mirror `Settings.inner.tools/commands.disabled` (`store.rs:137-138`). Default-pruning item unchanged. |

## Group E — codex/shell/jobs/agent (15 findings: 7 VERIFIED, 5 PARTIAL, 1 REFUTED, 2 verified-no-action)

| ID | Verdict | Notes / corrections folded in |
|---|---|---|
| CDX-1 | VERIFIED | Both dev-debug subsystems gated on env vars that appear nowhere else; duplicated image diagnostics vs `lua_tool.rs`. |
| CDX-2 | **PARTIAL** | Framing was wrong: `openai_compat/mod.rs:562-564` has the **identical** no-op `validate()`. `anthropic.rs` has no override; only `grok_build.rs:165-167` is real. Both no-ops are deletable (fix expanded to cover both files). |
| CDX-3 | VERIFIED (keep) | Low-confidence fallback; 6 lines, cheap to keep — owner judgment stands. |
| CDX-4 | VERIFIED (no action) | Request-identity mirroring is intentional and documented; flagged as concentration only. |
| JBS-1 | **PARTIAL** | Seven unscoped wrappers are test-only, but the test caller set is **three** files, not two (`ext/jobs_tests.rs`, `rpc/rpc_tests.rs`, `tui/tests/subagent_test.rs`) — the fix must update all three. `version()` citation corrected: `rpc/mod.rs:1363` is the `JobRegistry` call; `rpc/mod.rs:1324` is the *processes* registry (different method); scoped sites `ctx.rs:3313` and `rpc/mod.rs:1368` were omitted from the original claim. |
| MISC-1 | **PARTIAL** | `current_unix_seconds` de-dup holds, but the TUI pane works on `bone_protocol::JobStatus`, not the core `JobStatus` — the `status_sym` import is **not** drop-in. |
| MISC-2 | VERIFIED | `SessionWriter` has no TUI usage. |
| MISC-3 | VERIFIED | `Default for JobRegistry` never called. |
| MISC-4 | VERIFIED | `NewJob` single call site. |
| MISC-5 | **PARTIAL** | `core/tests/codex_provider_test.rs:3` imports `build_codex_messages`, `build_instructions`, `codex_tools`, `CodexRequest`, and `CodexReasoning` through the public API — those stay `pub`. Only `CodexInputItem`/`CodexContent` are truly module-internal. |
| MISC-6 | VERIFIED | One-liner wrapper, one call site. |
| PVC-1 | **REFUTED** | See below. |
| SHELL-1 | **PARTIAL** | `ShellTool::execute` is production-dead (registry goes through `execute_output_live`) **but** has ~14 test callers in `core/tests/shell_test.rs` — the original "no callers" grep claim was wrong. **`run_script` is live production code**: `ctx.shell()` calls it at `ctx.rs:1578` (`run_script_lines` at `:1610`). Do **not** delete it. |
| SHELL-2 | **PARTIAL** | The "two layers collapse" conclusion is void — it rested on the false SHELL-1 premise, and `run_script_stream_with_metadata` is also called by `processes.rs:125`. The type-pair collapse (`DirectExecRequest` vs `ProcessRequest`) remains valid. |
| SINK-2 | **PARTIAL** | `UsageOnlySessionSink::with_db` is production-dead, **not** zero-callers: two integration tests call it (`session_sink_test.rs:177`, `driver_turn_test.rs:431`). |

## Key false positives caught

These are the claims that, had they been acted on as written, would have broken working functionality:

1. **PVC-1 (refutation).** `ProviderRequestContext.max_tokens` is **not** speculative — it is a live, tested Lua API: `ctx.llm.complete({max_tokens=N})` writes the field at `ext/ctx.rs:1908`; assertions at `ctx_tests.rs:2180-2196` and `driver_turn_test.rs:1525,1619,1687`; consumed by `openai_compat/mod.rs:591` and `anthropic.rs:300-302`. Original audit error: it checked only the struct-literal init sites (all `None`) and missed the later field write.
2. **SHELL-1 / `run_script`.** `run_script` is live — `ctx.shell()` (`ctx.rs:1578`) is a bundled Lua API that routes through it. Deleting it (as the original fix proposed) would have broken `ctx.shell`.
3. **RPC-6 / `attach_with_initial`.** The proposed single-`Result` collapse is not behavior-preserving: the inner `Err(String)` branch at 2 of the 3 call sites converts load-bearing errors to `ConversationLoadFailed`/`Status` while the connection continues. Sub-claim retracted.
4. **Systematic test-caller undercounting.** Several "zero callers" claims missed test callers: DB-3 (2), SINK-2 (2), JBS-1 (a whole third test file), DB-4/SINK-1 (3 test sinks), CTX-6 (3 test call sites), MISC-5 (public-API imports in `codex_provider_test.rs`). Every deletion in the cleanup plan must update its test callers in the same commit.
5. **MISC-5 / visibility.** Five items claimed module-internal are in fact part of the public API consumed by integration tests.

## "Verified clean" list

Spot-checked: all entries passed. One descriptive correction folded in: `LuaViewDiff` is **not** a 1:1 mirror of `ViewDiff` (no `SetTheme` variant; internally-tagged serde) — but the "not dead" conclusion stands.

## Impact on the cleanup plan

- **TL;DR adjusted:** ~1,500 → **~1,450** safe deletion lines (PVC-1 and `run_script` removed from the deletion set).
- **Cleanup step 3** excludes PVC-1; SHELL-1 becomes "execute only"; a standing rule was added: **every deletion must update its test callers in the same commit**.
- **DB-4/SINK-1** and **JBS-1** fixes are scoped larger than originally estimated (test sinks / third test file).
- **SHELL-2** consolidation shrinks to the type-pair collapse only.
- **CDX-2** now covers two files.
- No verified finding was a false positive at its core: all 27 VERIFIED findings stand as actionable, and the 13 PARTIAL ones stand with their corrected scope.

## Process note

Both the original audit and this verification pass were static: no source files were modified, no commits made, no builds or test runs. The corrections listed above were folded inline into `OVER_ENGINEERING_AUDIT.md` on 2026-09-01.
