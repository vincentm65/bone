# Simplification Findings — Verification of `over_engineering_plan.md`

**Goal given:** net reduction in lines of code, *without losing features*, while improving code quality.

**Method:** Each of the 14 claims in `over_engineering_plan.md` was checked against the actual source (call-site greps, type signatures, and in one case an applied-then-measured refactor). This file records what holds up and what doesn't.

## Bottom line

The plan claims **~3,861 lines removable (34%) with "no feature loss."** That number does not survive contact with the code. It decomposes into three buckets, none of which delivers what was promised:

| Bucket | Claimed lines | Reality |
|---|---|---|
| **Feature removal** (markdown #5, tool_display #6, edit_file fuzzy #11, config migrations #7, policy→YAML #10) | ~1,560 | Violates the "no feature loss" constraint. These *are* features. |
| **File-splitting** (ctx #1, types #2, app #3, stream #4, registry #9, codex #8) | ~2,000 | Relocates lines into new modules — **does not remove them**; module decls + imports + re-exports usually make the total *grow*. The "after" estimates are fiction. |
| **Genuine dedup / dead-code** (driver #12, provider #13, rpc #14) | ~210 | Real targets, but measured savings are ~0 or **negative** once Rust's signature/formatting overhead is counted (proven below). |

**Realistic net reduction achievable without losing features or hurting readability: roughly 0–50 lines, not 3,861.**

The plan conflates two legitimate but *opposed* goals: "improve code quality" (split big files — costs LOC) and "reduce LOC" (drop features — costs functionality). You cannot get 34% line reduction *and* keep every feature. The big files are large mostly because they carry genuine surface area (Lua bindings, event variants, provider wire formats), not because of removable boilerplate.

---

## Proof point: the cleanest "real dedup" was applied and measured

Plan #12 (`driver.rs`) flags a genuinely duplicated ~30-line token-usage tail (the `ChatEvent::TokenUsage` handler vs. the estimation fallback). This is the single most defensible dedup in the whole plan. I implemented it: extracted a shared `emit_usage(...)` helper, replaced both call sites, **build passed cleanly**.

Result: **704 → 718 lines (+14).** The helper needs 12 parameters (`llm`, `session`, `usage_records`, `on_token_usage`, `extensions`, `emit`, `stats` + 5 usage fields) because the two call sites share data but live in different borrow scopes. The wide signature + two call sites cost more lines than the duplicated body saved. **Reverted** to honor "net reduction only."

Takeaway: in idiomatic Rust, removing a <40-line duplication via a many-argument helper routinely *increases* LOC while improving DRY-ness. LOC and DRY-ness are not the same axis here.

---

## Per-item verdicts

### #1 `ext/ctx.rs` (2,134) — split into 7 sub-modules — ❌ no net reduction
Each `build_*_table` binds *distinct* Lua closures (`build_fs/ui/usage/conversation/state/tools/session/db/config_table`, `add_agent_table`). They share a shape ("make table, attach closures, return") but the bodies are the actual Lua API surface — irreducible. Splitting relocates lines and adds 7 `mod`/`use` headers. **Quality may improve (smaller files); LOC will not drop.** Not aligned with the stated goal.

### #2 `ext/types.rs` (859) — move + replace `lua_value_to_json` — ⚠️ mostly file-move
Moving `parse_lua_return_action` etc. to `actions.rs`/`ops_events.rs` is relocation, not reduction. The one concrete LOC claim — replace `lua_value_to_json` (16 lines, used 2×) with mlua's `LuaSerdeExt::from_value` — is **risky**: that function exists precisely because Lua tables can't distinguish array-vs-object, which `from_value` handles differently (tests use `from_value` only on already-typed data). Marginal (~10 lines) and behavior-sensitive. Skip.

### #3 `ui/app/mod.rs` (2,159) — group 38 fields into sub-structs — ❌ net *increase*
Grouping fields into `TurnState`/`SessionState`/`PaneState`/`StreamingState` adds 4 struct definitions, their construction, and rewrites every `self.field` to the longer `self.state.field`. With 38 fields used across hundreds of sites, this **adds** lines. It may aid organization but directly contradicts "net reduction."

### #4 `ui/app/stream/mod.rs` (1,236) — split into 6 modules — ❌ no net reduction
Same as #1: the `tokio::select!` loop, `KeySink`, approval/thinking/pane handling are real logic. Splitting relocates + adds module plumbing. The plan explicitly says the `KeySink` state machine is "moved, not simplified." No reduction.

### #5 `ui/render/markdown.rs` (799) — drop tables + syntax highlighting — ❌ FEATURE LOSS
Claimed ~549 lines, the plan's single biggest number. But these are features: `syntect` syntax highlighting (used in 6 sites) and box-drawing tables. Dropping them is a UX regression, explicitly excluded by "without losing features." Rejected.

### #6 `ui/tool_display.rs` (378) — drop heredoc reflow, truncate — ❌ FEATURE LOSS
The heredoc/code reflow is a real display feature. Replacing with truncation loses it. The mini-parser is arguably over-built, but the plan's fix *is* feature removal. Rejected as specified. (A reliability-only rewrite that keeps reflow is possible but unlikely to net-reduce.)

### #7 `config/custom.rs` (801) — delete migrations + dual-format — ❌ FEATURE LOSS (backward-compat)
The 5 migrations (`migrate_old_values_file`, `migrate_status_values_from_general`, `migrate_providers_file`, `backfill_*`) run on every load (lines 101–105) to upgrade configs from older versions. Deleting them breaks any user not yet migrated — that's dropping backward-compatibility, a feature. Safe *only* as a deliberate "we no longer support pre-2.x configs" decision (a product call, not a refactor). Dual-format consolidation has the same caveat.

### #8 `llm/providers/codex.rs` (570) — extract shared SSE module — ❌ no net reduction
The premise is wrong: codex **already** imports and reuses `PartialToolCall` + `flush_partial_tool_calls` from `openai_compat` (codex.rs:153). The sharing exists; the "coupling" is one `use`. Extracting a *third* module to hold already-shared code **adds** a file. The other suggestions (resolve API key at construction, named return type, `#[serde(tag)]` enum) are quality tweaks worth ~0 net lines.

### #9 `tools/registry.rs` (367) — extract `ExecutionPlanner` — ❌ no net reduction
Moving serial/parallel logic into a new struct/module is relocation + new plumbing. Removing the dual constructor (`new` vs `with_enabled_safety_and_display`) is a small real cleanup but a handful of lines at most.

### #10 `tools/command_policy/mod.rs` (413) — move dangerous rules to YAML — ⚠️ not a codebase reduction
Moving hardcoded dangerous-command rules from Rust into YAML trades Rust LOC for YAML LOC **plus** the Rust needed to interpret the new YAML rule shapes. Total repo lines likely flat, and it widens the behavior surface (new config schema). File-splitting the rest is relocation. Not a net win.

### #11 `tools/edit_file/mod.rs` (597) — remove fuzzy matching — ❌ FEATURE LOSS
~200 lines of fuzzy matching (`find_match_span`, `fuzzy_candidate`, `MatchSpan`, score/margin thresholds) exist to let edits land when the model's anchor is slightly off — reducing failed-edit retry loops. The plan frames removal as a "safety improvement," but it is a deliberate capability removal with a real cost (more model round-trips). Excluded by "without losing features."

### #12 `runtime/driver.rs` (704) — dedup `emit_usage` — ❌ measured net +14 (see proof above)
Real duplication, but extraction costs more lines than it saves. The other suggestions (`ToolExecConfig` param-bundle struct, fold `remit` into `emit_runtime`) are relocation/neutral.

### #13 `llm/provider.rs` (234) — remove "dead code" — ❌ claims inaccurate
Verified against source:
- `http_status_to_error_kind` takes a **`StatusCode`** (not `&str` as the plan says) and is genuinely shared by **both** codex and openai_compat (callers at codex.rs:378, openai_compat:451). It is *not* a duplicate of `From<reqwest::Error>` (different input type). **Keep.**
- `impl Error for LlmError` is **not dead** — it's what lets `LlmError` flow through `?`/`Box<dyn Error>`. Removing it risks breakage. **Keep.**
- `Reasoning { echo_field }` is **actively used** for round-tripping reasoning to providers (openai_compat:173, driver.rs:466/481). Collapsing to `Option<String>` loses `echo_field` — a feature. **Keep.**
- The 3 `ChatMessage` constructors reduce verbosity at 3+ call sites; consolidating makes callers *longer*. **Keep.**
- Only `ChatRole::as_str()` (1 caller, ctx.rs:652) is removable, saving ~8 lines — but inlining a named match *hurts* readability for a trivial gain. Not worth it.

### #14 `rpc/mod.rs` (264) — remove pump task + "Phase 5/6 comments" — ❌ claims fabricated
- There are **no "Phase 5"/"Phase 6" comments** in the file. That claim is invented.
- The intermediate pump task (rpc.rs:151) bridges the agent's `mpsc::UnboundedSender<RuntimeEvent>` to the hub's `broadcast` channel — **different channel types**. You can't "wire directly"; the bridge is required. The claim that `AgentRunEvent` being a `RuntimeEvent` alias makes it removable confuses the payload type with the transport type.
- `run_daemon` is already documented as a minimal POC; no speculative cruft to strip.

---

## Recommendation

1. **Drop the "34% reduction with no feature loss" framing — it is not achievable.** The number is built from feature removals and illusory file-moves.

2. **If the real goal is code *quality*** (the genuinely large files — ctx.rs, app/mod.rs, stream/mod.rs are legitimately hard to navigate), do the module splits **as quality work, expecting LOC to stay flat or rise slightly.** That's a fine investment; just don't sell it as line reduction.

3. **If the real goal is fewer *lines*,** the only meaningful levers are the feature removals (#5, #6, #11, and the compat drops in #7/#10) — each a deliberate product decision, not a "safe refactor." Pick them individually on their UX merits.

4. **Genuinely safe, net-reducing, quality-neutral-or-positive changes from this plan: essentially none.** The handful of dead-code claims (#13, #14) are mostly inaccurate; the dedups (#12) measure net-positive.

5. The most accurate one-line summary of the plan: *it correctly identifies which files are big, but mis-attributes the bigness to removable boilerplate when it's mostly irreducible surface area — and several specific claims (provider.rs dead code, rpc.rs comments/pump, codex shared module) don't match the code.*
