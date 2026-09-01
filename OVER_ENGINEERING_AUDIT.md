# Bone Core — Over-Engineering Audit

- **Date:** 2026-09-01
- **Scope:** The 11 largest non-test files in `core/src` (plus cross-references into `tui/`, `webui/`, `protocol/`, `npm/`, `core/defaults/`)
- **Method:** Five parallel reviewer agents, one per file group. Every "dead / unused" claim required grep verification across the whole workspace before being reported. Review-only; no files modified.
- **Status:** Independently verified 2026-09-01 (five fresh reviewer agents, one per group, instructed to distrust this document). Verdicts per finding ID in [`verification-report.md`](./verification-report.md); corrections folded inline below. **Tally: 27 VERIFIED · 13 PARTIALLY VERIFIED (corrected) · 1 REFUTED (PVC-1).**

## TL;DR

- **~1,450 lines of safe, zero-behavior-change deletions** (~13% of the audited files). Verification removed PVC-1 and `run_script` (both live) from the deletion set.
- **~600+ lines of consolidation/dedup** that is behavior-preserving but requires care.
- The recurring patterns: superseded migration paths left in place, test-only scaffolding, one-shot debugging machinery, single-call-site abstractions, and parallel representations of the same data.

---

## Findings

IDs are stable so the verification pass can reference them. Each finding lists: **claim**, **evidence** as reported by the reviewer, **proposed fix**, and **risk notes** where relevant.

### Group A — `core/src/ext/ctx.rs` (4,175 lines, 161 KB)

#### CTX-1 — Hand-rolled PNG codec (~550 lines) is test-only
- **Where:** `ctx.rs:29-388` (`decode_png_rgba`, `BoundedPngOutput`, `encode_png_rgba`, `fit_png_dimensions`, `box_resample_horizontal`, `box_resample_vertical` — two near-identical resamplers, ~60 lines each (~120 combined)) + `ctx.rs:1365-1564` (Lua bindings `ctx.codec.png_tiles` / `png_resize` / `png_region_sha256` / `png_diff` and `check_png_cancelled` plumbing).
- **Claim:** The only consumers are the codec's own unit tests (`core/src/ext/ctx_tests.rs:1105-~1795`). No bundled Lua, tool, command, or production path calls any `ctx.codec.png_*`.
- **Evidence reported:** grep over `core/defaults`, `core/tests`, `tui`, `webui`, `npm` for `png_tiles|png_resize|png_region_sha256|png_diff` returns zero non-test hits.
- **Fix:** delete the codec and its bindings. Reimplements an image library; the `png` crate is already a dependency for decoding.
- **Risk:** third-party *installed* plugins (outside this repo) could in theory use `ctx.codec.png_*`. If the catalog is clean, deletion is safe.

#### CTX-2 — Dead Lua API: entire `ctx.session` table (~98 lines)
- **Where:** `ctx.rs:2412-2509` (`build_session_table` building `ctx.session.current/list/messages`, including ~60 lines of hand-rolled JSON re-parsing of `tool_calls`/`images` columns at `:2466-2500`), wired at `ctx.rs:800` (`ctx.set("session", …)`).
- **Claim:** No consumer in the workspace; the bundled code has superseded it.
- **Evidence reported:** `core/defaults/lua/lib/history.lua:1-2` doc says "Replaces the former `ctx.session.list` / `ctx.session.messages` Rust helpers" and uses `ctx.db.query` (history.lua:44, 97). Repo-wide grep for `ctx.session.` over `*.lua`/`*.rs` finds only that comment and the builder itself. (Hits in `core/src/rpc/rpc_tests.rs` are the Rust `ctx.session` *struct field*, not the Lua table.)
- **Fix:** delete `build_session_table` and the `ctx.set("session", …)` line. `build_current_fn` stays for `ctx.conversation.current` (tested at `ctx_tests.rs:668`).
- **Risk:** same third-party-plugin caveat as CTX-1 (it is part of the public extension surface).

#### CTX-3 — Dead field: `CtxConfig.turn_nudge` / `AppCtxState.turn_nudge`
- **Where:** `ctx.rs:544, 581, 609, 630, 646, 668`.
- **Claim:** `apply_to` copies `turn_nudge` into `CtxConfig`, but `create_ctx_table` and every builder never read it. The nudge is actually delivered through the shared `Arc<Mutex<Option<String>>>` (consumed at `core/src/runtime/driver.rs:777`, flushed at `core/src/rpc/mod.rs:3077`). The 13th parameter of `AppCtxState::new` (`ctx.rs:630`) is dead plumbing; its doc comment ("Passed to the `before_turn` hook") describes behavior that doesn't exist.
- **Fix:** drop the field, the `AppCtxState` field, and the constructor param; keep the Arc path.

#### CTX-4 — Single-call-site abstraction + triple-duplicated token estimate
- **Where:** `ctx.rs:4035-4111` (`estimate_prompt_tokens`, `PromptTokenEstimate`, `build_usage_context`), `ctx.rs:2148`, and `ctx.rs:4063-4065`.
- **Claim:** (a) The three `pub(crate)` items are documented as "shared by every site" but have exactly one call site each: `AppCtxState::new` (`ctx.rs:632, 642`). (b) `estimate_tokens` (`ctx.rs:4063-4065`) duplicates `crate::agent::estimate_tokens` (`core/src/agent.rs:770-772`), and line 2148 inlines the same `(chars as f64 / CHARS_PER_TOKEN).ceil()` expression a third time.
- **Fix:** inline (a) into `AppCtxState::new`; use `agent::estimate_tokens` everywhere for (b).

#### CTX-5 — `ctx.settings` table has no consumer (~12 lines)
- **Where:** `ctx.rs:2586-2596` (`build_settings_table` forwarding to `bone.settings._get_extension`), wired at `ctx.rs:793-794`.
- **Claim:** No bundled Lua or test calls `ctx.settings.get`; the only textual reference in the codebase is the error string at `core/src/ext/api.rs:284` telling scripts to use it. Bundled Lua uses `bone.settings.get` directly (see `api_tests.rs:367-371`).
- **Fix:** delete the table + the `_get_extension` indirection; repoint the error message and update the `api_tests.rs:430` assertion (it asserts on the `_get_extension` error text).
- **Risk:** third-party plugins could use it (public surface).

#### CTX-6 — Pure forwarding: `build_config_table`
- **Where:** `ctx.rs:2932-2934`.
- **Claim:** One-line wrapper around `build_canonical_config_table` with no added behavior.
- **Fix:** call the canonical function directly at `ctx.rs:797`; delete the wrapper and retarget the 3 test call sites that invoke it (`ctx_tests.rs:218,295,321`) to `build_canonical_config_table`.

#### CTX-7 — Duplicated cancel-polling loops
- **Where:** `ctx.rs:1979-1988` (`wait_for_llm_cancel`) vs `ctx.rs:4025-4033` (`await_cancelled`).
- **Claim:** Two hand-rolled `while !flag.load() { sleep }` futures (25 ms vs 50 ms periods) doing the identical job.
- **Fix:** merge into one helper.

#### CTX-8 — Redundant DB guard: `is_allowed_db_query_prefix`
- **Where:** `ctx.rs:2511-2517`, used at `:2526`.
- **Claim:** The comment itself says "Actual write protection is enforced after prepare via `Statement::readonly()`" (`ctx.rs:2554`), which alone rejects any non-readonly statement including `WITH … INSERT`. The prefix pre-check is a second, weaker gate that can only change the error message.
- **Fix:** delete the prefix pre-check; keep `readonly()`.

#### CTX-9 — Minor vestigial items
- `ctx.runtime.info().execution` subtable (`ctx.rs:1839-1842`): `kind` is the hardcoded constant `"agent"` and `depth` duplicates the top-level `agent_depth` field. Pinned only by its own test (`ctx_tests.rs:1918-1919`) plus a second test-only consumer, the fixture `core/tests/fixtures/task_list.lua:323`; `driver_turn_test.rs` reads only the top-level fields. Candidate for removal.
- `tostring_lua_value` `Value::Nil => "null"` (`ctx.rs:4133`): unreachable — the sole caller maps `Nil` → `Null` at `ctx.rs:2542` before the fallback arm.
- `#[cfg_attr(not(feature = "tui"), allow(dead_code))]` on `usage_by_provider_context` (`ctx.rs:4070`): the `tui` feature only enables crossterm (`core/Cargo.toml:37-38`) and the calling `rpc` module is unconditionally compiled (`lib.rs:13`) — the attribute can never matter.
- `pub use bone_protocol::UsageProviderContext;` (`ctx.rs:456`): referenced only inside ctx.rs itself and `ctx_tests.rs:402,438` (via `use super::*`).
- `config_store`/`config_schema` (`ctx.rs:533-534`): always set together at every construction site (`run.rs:206-207`, `rpc/mod.rs` via `apply_to`, `lua_tool.rs:216-224`, `ctx.rs:4117` `build_before_turn_config`), yet typed `Option`; `create_ctx_table` hard-errors on missing store (`:732`) and `build_canonical_config_table` `expect()`s the schema (`:2688-2692`). Making them required values eliminates the panic-by-`expect` landmine.

### Group B — `core/src/rpc/mod.rs` (3,348 lines, 135 KB) + `rpc/codec.rs`

#### RPC-1 — Dead wire method: `RuntimeCommand::SetSetting`
- **Where:** `rpc/mod.rs:2656-2664` (idle arm), `:3024-3031` (mid-turn arm), `:877` (`is_config_command`), and the private helper `DaemonCtx::set_extension_setting` at `:1463-1473`, which exists only to serve this command.
- **Claim:** No sender exists in the workspace.
- **Evidence reported:** `grep -rn "SetSetting"` over `core/tests tui webui npm` → zero hits; `grep -rn "set_setting"` → only `webui/tests/ux.test.mjs:118`, which asserts `set_setting` is *absent* from the web UI. The webui's revisioned config commands (bridge.mjs, app.js) send `set_tool_enabled`, `upsert_subagent`, `delete_provider`, etc., but never `set_setting`. No Lua `ctx` API constructs a `RuntimeCommand` either.
- **Fix:** delete both match arms, the `is_config_command` entry, and `set_extension_setting`; drop the `SetSetting` variant from the `RuntimeCommand` enum (and the `runtime/conn.rs:179` reference).

#### RPC-2 — Triple-duplicated command pump (~150 lines of copy-paste)
- **Where:** `DaemonCtx::run_managed_hook` (`rpc/mod.rs:1561-1652`), `DaemonCtx::run_interactive_command` (`:1728-1879`), and the select loop in `DaemonCtx::run_turn` (`:2963-3063`).
- **Claim:** The same arms are copied across all three *(verified correction: only 5 of the 7 arms claimed are literally present in all three pumps — the `status_rx`/`live_rx` and `ApprovalReply`/`KeyReply` arms exist only in the hook/command pumps; `run_turn` uses `background_events_rx`/`conn.next_event`/`conn.send` instead)*: the `status_rx.recv()` publish arm, the `live_rx.recv()` KeyRequest arm, the 50 ms `diff_timer` tick arm (`publish_processes`/`publish_jobs`/`drain_diffs`), the `ApprovalReply`/`KeyReply` registry-resolve arms, the `Synchronize`/`GetProcesses`/`GetJobs`/`CancelProcess` arms, the `is_config_command` recursive `handle_idle_command` arm, and the `pending_commands.push_back` catch-all. The post-completion cleanup sequence (`record_private_llm_usage` → `pending_interactions.clear` → `drain_diffs`) is repeated 5 times (2 in `run_managed_hook`, 3 in `run_interactive_command`), with the exact ordering verbatim only in the cancel/none arms.
- **Fix:** factor one shared "pump while a blocking task runs" helper parameterized by the blocking handle plus a small callback for the differing arms (turn-only arms like `Steer`/`Cancel`/`SubmitPrompt`-steer; hook/command-only arms like `Cancel`/`None`-shutdown). The two `BlockingCtxSetup`-based pumps could share a single implementation.
- **Note:** behavior-preserving refactor, not a deletion.

#### RPC-3 — Speculative genericity: `impl Into<HubPublisher>` + `From<Hub> for HubPublisher`
- **Where:** `rpc/mod.rs:155-165` (`From` impl), `:3118` and `:3149` (`hub: impl Into<HubPublisher>` on `run_daemon`/`run_daemon_with_projection`), `:3131`/`:3161` (`hub.into()`).
- **Claim:** Every one of the 13+ call sites (`tui/src/main.rs:355,799`; `core/src/rpc/rpc_tests.rs:863,2097`; `core/tests/{daemon_subagents,interactive_esc,remote_config_snapshot,rpc_daemon}_test.rs`) passes `hub.publisher()` — already a `HubPublisher`. Nothing ever converts a `Hub`. The comment at `:155-158` documents a conversion path nobody takes.
- **Fix:** type both parameters as `HubPublisher`; delete the `From` impl.

#### RPC-4 — Hand-rolled byte-budgeter `bounded_jobs_snapshot`
- **Where:** `rpc/mod.rs:1015-1058`.
- **Claim:** To keep a `JobsSnapshot` under `MAX_LINE_BYTES` it (a) re-serializes the entire snapshot in an outer `loop` on every iteration, (b) removes events one at a time, re-serializing each removed event to measure its size, (c) contains an `unreachable!()` on a just-constructed value, (d) has a subtle terminal condition (`if removed_bytes == 0 && jobs.pop().is_none()`). Worst case O(n) full re-encodes per trim pass, on the 50 ms `diff_timer` tick path (`publish_jobs`, `rpc/mod.rs:1360-1378`).
- **Fix:** a single pass — compute encoded size once, then drop oldest events (or cap event count per job) until under budget, or cap `events.len()` and let the codec's write-side limit be the backstop.

#### RPC-5 — `ReadError` classifies recoverability twice
- **Where:** `rpc/codec.rs:28-44`.
- **Claim:** Both `is_recoverable()` and `into_fatal_io()` independently encode the same "only `Decode` is recoverable" rule. Each has exactly one call-site group: `is_recoverable` at `core/src/runtime/conn.rs:279`; `into_fatal_io` at `rpc/mod.rs:584` and `:790`.
- **Fix:** keep `into_fatal_io` and derive `is_recoverable` from it (or replace the conn.rs guard with `err.into_fatal_io().is_none()`).

#### RPC-6 — Minor indirections
- `attach_with_initial` (`rpc/mod.rs:506-519`) returns `std::io::Result<Result<SessionAttachment, String>>`. **Sub-claim retracted by verification:** it has 3 call sites (`:535, :543, :556`), and the inner `Err(String)` path is load-bearing at 2 of them (it converts to `ConversationLoadFailed`/`Status` while the connection continues) — collapsing to a single `Result` is **not** behavior-preserving. Keep as-is.
- `SessionRequest` (`rpc/mod.rs:356-361`) is a one-variant enum wrapping `(SessionTarget, oneshot::Sender<…>)`; a plain struct message is equivalent.
- `finish_subagent_change` (`rpc/mod.rs:1454-1461`) is a three-argument specialization of `finish_config_mutation` used in 3 arms; inlinable.

### Group C — `core/src/session_db.rs` (1,846 lines) + `core/src/runtime/driver.rs` (1,748 lines)

#### DB-1 — FTS5 table `messages_fts` written on every message, never queried in production
- **Where:** `session_db.rs:360-365` (schema), `:784-794` (v6→v7 rebuild migration), `:1005-1008` (per-message insert in `insert_message_row`).
- **Claim:** `grep -rn 'messages_fts'` shows zero SELECT/MATCH queries in production code — the only readers are `session_db_tests.rs:379,704,713`. There is no search function in `SessionDb` at all. Smoking gun: `session_db.rs:1707` has an orphaned doc comment `/// Full-text search across all conversations.` sitting directly above `list_conversations` — the search function was deleted; the table and its write/maintenance machinery were left behind.
- **Cost:** one extra FTS write per message (same transaction), a ~15-line rebuild migration that runs on every v6→v7 database, and schema surface.
- **Caveat:** Lua `ctx.db.query` (`ext/ctx.rs:2540+`, read-only SELECT) *can* query it if a third-party extension wants to; `list_messages` (Lua `/history`) reads normalized columns, not FTS. This is "no first-party consumer," not "impossible to reach."
- **Fix:** drop `messages_fts` from `FULL_SCHEMA` and `insert_message_row`; replace the v6→v7 rebuild with a no-op version bump. If search is wanted, re-add it as one function that actually reads the table.

#### DB-2 — `SessionDb::append_turn` (no-checkpoint wrapper) dead in production
- **Where:** `session_db.rs:1144-1152`.
- **Claim:** Forwards to `append_turn_with_checkpoint(..., None)`. Only callers are `session_db_tests.rs:313,517,533`. Production (`session.rs:381,474`) always calls `append_turn_with_checkpoint` directly.
- **Fix:** delete; point the three tests at `append_turn_with_checkpoint(..., None)`.

#### DB-3 — `StartupDbError::is_transient_contention` dead
- **Where:** `session_db.rs:523-530`.
- **Claim (corrected):** production-dead, not zero-callers — the only callers are two test sites (`session_db_tests.rs:1367,1393`). (`sqlite_codes` at `:514` *is* used by `Display`; keep it.)
- **Fix:** delete the method and update its two test callers in the same commit.

#### DB-4 / SINK-1 — Vestigial 9-arg "normalized projection" write path (cross-file; independently confirmed by two reviewers)
- **Where:** `core/src/session_sink.rs:36-67` (trait `append_message` + default `append_chat_message`), `core/src/session_db.rs:1083-1137` (`SessionDb::append_message`, 55 lines), `core/src/agent.rs:119-146` (`SessionWriter::append_message`), `core/src/runtime/session.rs:321-340`.
- **Claim:** A legacy 9-argument stringly-typed `append_message(role, content, tool_name, tool_call_id, tool_calls, images, is_error, seq)` exists in the `SessionSink` trait, in `SessionDb`, and in `SessionWriter`, alongside the lossless typed `append_chat_message(&ChatMessage, i64)`. No production code calls the trait `append_message` directly: the Driver exclusively calls `append_chat_message` (`runtime/driver.rs:123,581,592,1307,1332,1484,1506`). `SessionWriter` overrides `append_chat_message`, so its `append_message` is only reachable via the trait default, which nobody invokes on it. The only remaining production call to `db.append_message` is `session.rs:327` — a fallback for "caller supplies malformed JSON," where the JSON came from `serde_json::to_string` in-process (the driver serializes `tool_calls`/`images` itself), i.e. a state that cannot occur.
- **Claim (justification audit):** The trait docs call this a "stable implementation surface for third-party sinks," but `bone-core` is an unpublished local crate (no `publish`/`repository` in `core/Cargo.toml`) and every `SessionSink` impl is in-repo — **six** total, not three: the 3 production ones (`NullSessionSink` (no-op), `UsageOnlySessionSink` (no-op for this method), `SessionWriter`) plus 3 test sinks (`agent_tests.rs:41`, `session_sink_test.rs:43`, `driver_turn_test.rs:1034`) that exercise the trait-default projection.
- **Important:** The *normalized columns themselves* are still needed (read by `list_messages`/Lua `/history` and as the v9-or-older fallback in `stored_to_chat_message`), and `insert_chat_message_row` fills them — keep all of that. What's dead is the separate 9-arg *write API* over the same columns.
- **Fix:** make `append_chat_message(&ChatMessage, i64)` the single required `SessionSink` method; delete the trait `append_message` + default, `SessionDb::append_message`, `SessionWriter::append_message`, and the malformed-JSON branch in `session.rs` — the change is larger than the ~130-line estimate because the 3 test sinks above also exercise the trait default and must be updated.

#### DRV-1 — Private-usage recording duplicated verbatim in driver.rs
- **Where:** `runtime/driver.rs:70-104` (`record_hook_usage`) vs `:881-912` (inline loop after the `before_turn` hook).
- **Claim:** The inline loop does exactly what `record_hook_usage` does — `token_stats.record_request`, `session.record_usage`, `usage_records.push(UsageRecord{...})` — differing only in the provider/model source and an extra `report_usage(&token_stats)` per record.
- **Fix:** extract `report_usage` into a closure the helper takes (or have `record_hook_usage` emit it) and call the helper at `:881` instead of the 30-line inline copy.

#### DRV-2 — `before_turn` hook machinery reimplemented inline (~200 lines)
- **Where:** `runtime/driver.rs:787-1007` vs `:192-303`.
- **Claim:** Every other hook (message, session_start, turn_start, token_usage, tool_call, tool_result, turn_end, session_end) goes through `DriverHookRuntime::run`, but `before_turn` re-inlines the same pipeline: `AppCtxState::new`, `build_before_turn_config`, wiring `runtime_status`/`cancelled`/`approval_gate`/`agent_depth`, `PrivateLlmContext` construction (near-exact copy of `:222-232`), `spawn_blocking` dispatch, the cancel `select!` with the 100 ms grace timeout (copy of `:263-279`), private-usage drain (see DRV-1), and operation handling with the same warning string duplicated at `:131-133` and `:922-924` (`apply_hook_operations` then re-runs the Append branch at `:967-978`).
- **Impact:** a behavior change to hook dispatch (timeout, cancel semantics, ctx wiring) must be made in two places; the 14-field `DriverHookState` struct literal is repeated 8 times, and `before_turn` uses none of its fields (it inlines the whole pipeline instead).
- **Fix:** route `before_turn` through `DriverHookRuntime::run` with an optional "extras" out-param (sys_appends / tool_filter / replacement); let `apply_hook_operations` be the single owner of operation handling (drop the manual `pending_appends` collection at `:916-927`).
- **Confidence:** medium — the split may be intentional (before_turn needs extra outputs); this is consolidation, not deletion.

#### DRV-3 — Minor
- `runtime/driver.rs:66-68`: `restore_ephemeral_image_relays` is a one-line wrapper around `Vec::extend`. Inline the 2 call sites (`:963, :983`).
- `session_db.rs:1339`: `#[cfg_attr(not(feature = "tui"), allow(dead_code))]` on `max_message_seq` is stale/confusing — the function *is* used from core itself (`session.rs:181,254` and `rpc/mod.rs:1971`), so the attribute documents a false assumption.
- `session_db.rs:92-93`: dangling doc comment `/// A search hit from FTS5 query.` above `ConversationSummary` (remnant of the deleted FTS search; see DB-1).

### Group D — `core/src/config/settings.rs` (1,262 lines) + `core/src/config/store.rs` (927 lines)

`store.rs` is lean: every `pub fn` has ≥1 non-test caller; no dead code. The problems are concentrated in `settings.rs` (the `pub` visibility is why the compiler never flags them — core is a lib).

#### CFG-1 — Dead "legacy canonical keys" cluster (~270 lines)
- **Where:** `settings.rs:752-855` and `:911-1049` — the entire `// ── Canonical key routing (legacy page keys → new hierarchy) ──` block.
- **Claim (per item):**
  - `canonical_keys()` (`:755`) — only referenced by `is_canonical` (`:786`). No external caller. **Dead.**
  - `is_canonical()` (`:785`) — zero callers in the whole repo. **Dead.**
  - `get_value(ns, key)` (`:792`) — zero callers (grep `\.get_value(` over all `.rs` returns only the definition). **Dead.**
  - `get_ui(key)` (`:961`) — private, called only at `:803` inside `get_value`. **Dead transitively.**
  - `cycle_field()` (`:912`) — zero callers. The Lua `ctx.config.cycle_field` binding (`ext/ctx.rs:2775`) reimplements cycling inline from the schema's `options`; it never calls this method. **Dead.**
  - `set_value(ns, key, String)` (`:809`) — **test-only.** Production mutations go through `ConfigStore::set_value` → `Settings::set_path_at` (`store.rs:729`). Only call sites: `store_tests.rs:404`, `rpc_tests.rs:2281`, both `.set_value("general","show_thinking","true".into())`.
  - `set_ui(key, value)` (`:982`) — private, called only at `:846` inside `set_value`. **Test-only transitively.**
  - `settings_tests.rs` references none of these.
- **Claim (live path):** The live code path is the dotted-path `get_path`/`set_path_at` (`settings.rs:858, 867`) plus the schema-driven Lua binding in `ext/ctx.rs:2744-2799`.
- **Fix:** delete `canonical_keys`, `is_canonical`, `get_value`, `get_ui`, `cycle_field` (~150 lines). Point the two tests at `set_path_at` (or a small test helper) and delete `set_value`/`set_ui` (~120 lines). Total ~270 of the file's 1,262 lines, no behavioral change.

#### CFG-2 — `ThemeSettings` carries two parallel representations of the same 26 colors
- **Where:** `settings.rs:397-459` (`ThemeSettings`); nested structs `shell: ThemeShellSettings` (8 fields, `:344`) and `syntax: ThemeSyntaxSettings` (18 fields, `:357`); flat fields `shell_program…shell_path` (`:419-426`) and `syntax_text…syntax_invalid` (`:441-458`).
- **Claim:** Both are deserialized (schema allows either) and **both are applied** in `tui/src/ui/theme.rs::apply_snapshot`: nested `snap.shell.program` (`theme.rs:543-550`) then flat `snap.shell_program` (`:580-587`), and nested `snap.syntax.text` (`:552-569`) then flat `snap.syntax_text` (`:602-619`). The flat pass runs after the nested pass and overwrites it — a theme author has two spellings of the same value, and the code carries two deserialization + two application paths for 26 identical roles. Docs describe both (`configuration.md:89` `shell:` vs `:138` `shell_program`).
- **Fix:** pick one representation and drop the other. The nested `shell`/`syntax` structs are the cleaner choice (consistent with `palette` and `highlights`). Remove the 26 flat fields (`settings.rs:419-426, 441-458`) plus the 26 redundant `apply_ref!` lines (`theme.rs:580-587, 602-619`) and the doc entries.
- **Confidence:** medium — themes are user-supplied, so a real theme file *might* use the flat form (note: no bundled theme exists in the repo — `config.yaml` has no `theme` key); but the schema supporting two spellings of the same value is redundant by construction.

#### CFG-3 — `Settings::into_resolved` dead
- **Where:** `settings.rs:590`.
- **Claim:** Zero callers (grep `into_resolved` returns only the definition). `resolved()` (`:582`) is the live accessor.
- **Fix:** delete.

#### CFG-4 — Lower-priority smells (owner judgment)
- **Hand-rolled default-pruning:** `sparse_settings_value` + `prune_defaults` (`settings.rs:514-560`, ~45 lines). Exercised on every save (not dead), but it's custom recursive value-diffing with special cases for `version`/`general.system_prompt`. If "sparse config.yaml" is not a hard requirement, plain `serde_yaml::to_string` with `skip_serializing_if` on the fields would replace most of it.
- **Duplicate ownership (wider than first reported):** `Settings` vs `ConfigStore::Inner` (`store.rs:34-42`). `Inner` holds `core: Settings` *and* separate `subagents`/`extension_values`/`disabled_tools`/`disabled_commands`, while `Settings` also carries `subagents`/`extensions` (populated via `replace_domains`) *and* `tools`/`commands.disabled`, which `Inner.disabled_tools`/`Inner.disabled_commands` mirror (`store.rs:137-138`). Same data has two homes; `runtime_settings()` (`store.rs:203`) has to re-merge them. Not removable without care; design smell.

### Group E — `llm/providers/codex.rs` (1,052 lines), `tools/shell.rs` (882 lines), `ext/jobs.rs` (781 lines), `agent.rs` (845 lines)

#### CDX-1 — Two undiagnosed dev-debug subsystems in codex.rs (~185 lines, ~18% of the file)
- **Where:** `codex.rs:532-540` (`codex_debug_enabled`), `:542-606` (`debug_request_images`), `:608-659` (`CODEX_DEBUG_SEQ` + `codex_debug_log_line` + `codex_debug_dump_request` + `codex_debug_log_usage`), plus call sites at `:736, 740, 1011-1014`.
- **Claim:** Gated on env vars `BONE_CODEX_DEBUG` / `BONE_IMAGE_DEBUG` that appear **nowhere else in the repo** — no README mention, no tests, no docs. One-off debugging machinery for prefix-cache divergence and image sizing. `debug_request_images` additionally duplicates the same `BONE_IMAGE_DEBUG`-gated image diagnostics already implemented in `core/src/ext/lua_tool.rs:378-390` (two divergent implementations of the same diagnostic).
- **Fix:** delete both blocks and their call sites. Trivially recoverable from git history. Bonus: removes the `base64` + `AtomicU64` usage from this file.

#### JBS-1 — Seven unscoped `JobRegistry` methods used only by tests
- **Where:** `ext/jobs.rs` — `complete` (`:302-305`), `cancel` (`:550-552`), `wait_for` (`:398-405`), `running_ids` (`:356-362`), `running_jobs` (`:374-380`), `peek_finished_unconsumed` (`:513-526`), `snapshot` (`:484-492`).
- **Claim:** Every production call site uses the scoped variant:
  - `ctx.rs:3329` → `wait_for_scoped`; `ctx.rs:3355` → `cancel_scoped`; `ctx.rs:3283` → `snapshot_scoped`; `ctx.rs:3449,3512` → `complete_with_tokens`
  - `rpc/mod.rs:1661,1680,1697,1701` → `cancel_all_scoped`, `cancel_scoped`, `peek_finished_unconsumed_scoped`, `running_jobs_scoped`
  - The only callers of the unscoped forms are test files: `ext/jobs_tests.rs`, `rpc/rpc_tests.rs`, and `tui/tests/subagent_test.rs`.
- **Why it matters:** this is the reason `wait_for_matching` / `cancel_matching` / `scope_matches` carry a `required_scope: Option<Option<i64>>` parameter — a double-Option indirection whose only consumer is the test-only unscoped path.
- **Fix:** delete the seven wrappers and the private `cancel_matching`/`wait_for_matching` split; fold the scope filter directly into the scoped methods as a plain `Option<i64>`. Tests exercising "unscoped" semantics can pass the real scope (or `Some(None)`); the fix must update all three test files named above.
- **Note (corrected):** `version()` is **not** dead — `rpc/mod.rs:1363` is the `JobRegistry` call (snapshot-cache invalidation; `rpc/mod.rs:1324` is the *processes* registry, a different method), and the scoped form is also called at `ctx.rs:3313` and `rpc/mod.rs:1368`.

#### SINK-2 — `UsageOnlySessionSink::with_db` dead in production
- **Where:** `session_sink.rs:171`.
- **Claim (corrected):** production-dead, but not zero-callers — two integration tests call it (`session_sink_test.rs:177`, `driver_turn_test.rs:431`); the other `with_db` hits are the unrelated `HostService::with_db_path`. Only `for_parent` (used at `ext/ctx.rs:3608`) is live.
- **Fix:** delete the constructor (3 lines) and update the two test callers in the same commit.

#### PVC-1 — `ProviderRequestContext.max_tokens` — **REFUTED by verification; do NOT delete**
- **Where:** `core/src/llm/provider.rs:188-190`.
- **Claim (refuted):** the field is a **live, tested Lua API**: `ctx.llm.complete({max_tokens=N})` writes it at `ext/ctx.rs:1908`; it is asserted in `ctx_tests.rs:2180-2196` and `driver_turn_test.rs:1525,1619,1687`; it is consumed by `openai_compat/mod.rs:591` and `anthropic.rs:300-302`.
- **Original error:** the audit checked only the struct-literal init sites (all of which pass `None`) and missed the later field write.
- **Fix:** none — keep the field as-is.

#### CDX-2 — Redundant no-op `validate()` overrides (codex.rs + openai_compat)
- **Where:** `codex.rs:687-689` and `llm/providers/openai_compat/mod.rs:562-564`.
- **Claim (corrected):** Both providers override `validate()` with the identical no-op `Ok(())` — byte-for-byte the trait default (`provider.rs:220-222`). `anthropic.rs` has no override; only `grok_build.rs:165-167` implements real validation.
- **Fix:** delete both no-op overrides.

#### SHELL-1 — Unreachable `ShellTool::execute` override (the `run_script` part is refuted)
- **Where:** `tools/shell.rs:785-807` (`ShellTool::execute`); `run_script` at `:193-195`.
- **Claim (partly refuted):** `ShellTool::execute` is production-dead — the tool registry invokes tools only via `execute_output_live` (`tools/registry.rs:86`), which `ShellTool` overrides (`:809-858`). However, it has ~14 test callers in `core/tests/shell_test.rs` (the original "no callers" grep claim was wrong), so deleting it requires retargeting those tests. **`run_script` is live production code — do NOT delete:** `ctx.shell()` calls it at `ctx.rs:1578` (`run_script_lines` at `:1610`).
- **Fix:** delete only `ShellTool::execute` (retargeting its test callers in the same commit); keep `run_script`.

#### SHELL-2 — Parallel request/output type pairs + three-layer run stack
- **Where:** `tools/shell.rs:46-70` (`DirectExecRequest`/`DirectExecOutput`) wrap `:92-108` (`ProcessRequest`/`ProcessOutput`); the only difference is `max_output_bytes`, and the only converter is `run_direct_exec` (`:359-399`). Run stack: `run_script → run_script_stream → run_script_stream_with_metadata → run_process_stream` (`:193-195, 403-425, 428-483`).
- **Claim:** One type pair + a closure (which `run_process_stream` already has) suffices. The middle layer exists only to re-wrap `cancelled`/`timed_out` into an `Err` for non-live callers. *(Corrected:)* the "two of the layers collapse" conclusion is void — it rested on the false SHELL-1 premise that `run_script` is dead, and `run_script_stream_with_metadata` is also called by `processes.rs:125`. What survives is the type-pair collapse (one of `DirectExecRequest`/`ProcessRequest`).

#### MISC-1 — Duplicated helpers in TUI
- **Where:** `core/src/ext/jobs.rs:662` (`pub fn current_unix_seconds`, used only inside jobs.rs) and `tui/src/ui/jobs_pane.rs:232` (TUI re-implements the same 5-line helper), plus status glyphs at `jobs_pane.rs:200-201` duplicating `status_sym` (`jobs.rs:688-695`, which is `pub` but used only inside jobs.rs).
- **Claim:** `tui/Cargo.toml:15` already depends on `bone-core`; the TUI copy exists because nobody reused the core export.
- **Fix (corrected):** keep one `current_unix_seconds` in `bone_core` and import it in `tui/src/ui/jobs_pane.rs`; the TUI pane works on `bone_protocol::JobStatus`, not the core `JobStatus`, so the `status_sym` import is **not** drop-in (keep or adapt the TUI copy). Make `MAX_INJECT_CHARS` (`jobs.rs:14`, pub, used only in jobs.rs + its tests) private.

#### MISC-2 — `agent.rs:31` `pub struct SessionWriter` visibility
- **Claim:** Doc says "Public so the TUI can hand the runtime Driver a sink… The TUI itself does not use this." No TUI usage exists (grep: only `agent.rs` constructs it). Make `pub(crate)`.

#### MISC-3 — `jobs.rs:132-136` `impl Default for JobRegistry` never called
- **Claim:** Production uses `registry()`'s `OnceLock` + `new()`; tests use `new()`.

#### MISC-4 — `jobs.rs:94-107` `NewJob` params struct, single call site
- **Claim:** Only constructed at `ext/ctx.rs:3413`. Doc justifies it with "new fields (e.g. Tier 3 scope) don't grow the positional argument list" — speculative. A 6-field tuple at one call site is simpler; the struct is at most neutral value.

#### MISC-5 — `pub` on module-private wire types in codex.rs
- **Claim (corrected):** `core/tests/codex_provider_test.rs:3` imports `build_codex_messages`, `build_instructions`, `codex_tools`, `CodexRequest`, and `CodexReasoning` through the public API — those must stay `pub`. `build_codex_messages` is also consumed at `chat.rs:198`. Only `CodexInputItem`/`CodexContent` are truly module-internal; make just those non-`pub`.

#### MISC-6 — `codex.rs:64-66` `service_tier()` one-liner wrapper
- **Claim:** `self.fast_mode.then_some(FAST_SERVICE_TIER)` wrapped in a method with one call site. Inline it.

#### CDX-3 — (low confidence, owner judgment) `resolve_event_type` SSE fallback
- **Where:** `codex.rs:478-483`.
- **Claim:** "Some backends only set the event type in the SSE event line" — if the Codex Responses API always carries JSON `type`, the `sse_event` fallback branch never fires. Cannot be verified without a non-conformant backend. 6 lines; cheap to keep.

#### CDX-4 — (concentration note, not dead) Codex-CLI request-identity mirroring
- **Where:** `codex.rs:492-530` (`codex_session_id`/`codex_scope_id`), `prompt_cache_key`, `originator`/`session-id`/`thread-id`/`x-client-request-id` headers (`:749-764`), and the `x-codex-turn-state` round-trip (`turn_state` `OnceLock` in `ProviderRequestContext`, response-header capture at `:774-782`).
- **Claim:** Intentional and documented (cache-shard pinning, measured cache-miss oscillation). Flagged only as a concentration: if the backend ever stops honoring these headers, ~80 lines across `codex.rs` and `provider.rs` become dead. Worth a time-boxed verification that the headers still change billing behavior before the next backend change.

---

## Verified clean (checked and cleared — no action)

- **rpc/mod.rs:** all other `pub` items, both `run_daemon` variants, `RemoteClient`, `SessionManager(Receiver)`, `Hub::new`/`new_grouped`, `client_count`, `begin_turn`/`TurnGuard`, `publish_global`, `frontend_state`, `view_snapshot`, `MAX_LINE_BYTES`, all `SessionTarget` variants. Both `TurnComplete` and `TurnCompleted` events are consumed by the TUI stream loop (`tui/src/ui/app/stream/mod.rs:565-595`). No macro magic; codec generics have multiple concrete instantiations. The hand-rolled line reader in `MessageReader` is justified (tokio's `read_until` can't reject an oversized frame before the newline arrives).
- **ext/ctx.rs:** `LuaViewDiff` (`:1669-1692`) is **not** a 1:1 mirror of `ViewDiff` (no `SetTheme` variant; internally-tagged serde) but earns its keep as the serde deserialization bridge for `ctx.ui.apply` — the "not dead" conclusion stands; `block_on` (`:394`) has external users (`ops_plugins.rs`).
- **session_db.rs:** `db_path()` + legacy-migration chain (the one-shot XDG migration is load-bearing); `ViewMode` and all its methods (consumed by `tui/src/ui/stats.rs` and `protocol/src/host.rs:109,123`); the full `SessionDb` query API — `latest_conversation`, `conversation_exists`, `conversation_provider_model`, `set_conversation_provider`, `create_conversation_for_startup`, `record_usage`, `end_conversation`, `reopen_conversation`, `conversation_usage`, `usage_by_provider`, `usage_stats_snapshot`, `usage_stats_range`, `list_conversations`, `list_messages`, `load_messages`, `load_effective_transcript`, `conn_ref`, `stored_to_chat_message` — each has ≥1 production caller. `open_for_startup`/`retry_startup_sqlite`/`StartupDbOperation` are live (the busy_timeout retry is redundant defense but documented and small). `PersistedMessageV2` versioning, `TimeWindow`, `SUM_COLS`/`BUCKET_*` constants, usage bucket queries all feed the stats dashboard.
- **driver.rs:** fields `events`/`event_sender`/`on_token_usage`/`activity`/`turn_nudge`/`into_turn_future`/`run`/`run_to_outcome` — each has live headless, TUI, or Lua call sites.
- **config:** all `SubagentSettings` fields (consumed by `collect_subagents`, `types.rs:247-255`); `ExtensionValue` and its variants (used throughout `settings_registry.rs`); `extension_value` / `set_extension_value_at` / `set_path_at` / `replace_theme_at` / `reset_path_at` / `shipped_system_prompt`; every `store.rs` `pub fn` (`schema`, `schema_for`, `provider_candidate_config`, `check_revision`, `apply_populated_onboarding`, both catalog initializers, `runtime_settings_handle`, etc.). `core/defaults/config.yaml` defines only `version` + `general.system_prompt`; both are parsed (no orphan YAML keys).
- **codex.rs:** `build_codex_messages` consumed at `chat.rs:198`; the request-identity headers are live (see CDX-4).

---

## Suggested cleanup order

1. **PNG codec** (CTX-1) — biggest single win, ~550 lines.
2. **Settings legacy keys** (CFG-1) — ~270 lines, mechanical.
3. **Small dead-code sweep:** RPC-1, DB-1, DB-2, DB-3, SINK-1/DB-4, SINK-2, CDX-1, CDX-2, SHELL-1 (execute only — `run_script` is live), MISC-2/3, CFG-3, CTX-3/4/5/6/7/8/9. (PVC-1 excluded — refuted, it is live.) **Every deletion must update its test callers in the same commit** (see the per-finding notes; several "dead" items are exercised by tests).
4. **Consolidations:** RPC-2 (pump), RPC-4 (byte-budgeter), DRV-1/DRV-2 (driver hooks), SHELL-2 (shell run stack), JBS-1 (scope indirection), CTX-9 last item (Option→required).
5. **Theme de-dup last** (CFG-2) — themes are user-supplied; needs a compat decision (flat spelling accepted during a deprecation window, or broken with a clear error).

Recommended process: one commit per finding, each independently revertable; `cargo build` + `cargo test` per commit.

---

## Verification

Independent verification pass: **completed 2026-09-01**. Five fresh reviewer agents (one per group) were instructed to distrust this document and re-checked every finding and every "verified clean" entry with greps across `core` (src/tests/defaults), `tui`, `webui`, `protocol`, and `npm`. Tally: **27 VERIFIED · 13 PARTIALLY VERIFIED (corrections folded inline above) · 1 REFUTED (PVC-1)**; all "verified clean" spot-checks passed, and no verified finding is a false positive at its core. Full per-finding verdicts, per-ID corrections, and the false positives caught: [`verification-report.md`](./verification-report.md). No code was modified in either pass.
