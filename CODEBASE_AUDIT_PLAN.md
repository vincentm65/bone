# Codebase Audit Plan

## Scope

Review the tracked product code outside `webui/`: the `protocol`, `core`, and `tui` crates; bundled Lua/configuration; tests; and release/package integrations. The current scope is approximately 62,000 lines across nine review sections.

Excluded:

- `webui/`
- `Cargo.lock` and generated build output
- Untracked recordings (`recording.mp4`, `recording-30fps.mp4`)
- Historical review findings as code to audit (`bonereview.md`, `grokreview.md`); use them only as leads and re-verify every claim

Review tests with the production code they exercise rather than as a separate final pass. This keeps each section independently actionable and exposes missing coverage while the relevant behavior is fresh.

## Review standard

Apply the same checks to every section:

1. **Correctness:** invariants, edge cases, state transitions, cancellation, retries, partial failures, and platform behavior.
2. **Security:** trust boundaries, input validation, path/process/network safety, secrets, permissions, and denial-of-service limits.
3. **Reliability:** error propagation, cleanup, atomicity, concurrency, panic behavior, persistence, and recovery.
4. **Architecture:** ownership, dependency direction, duplicated paths, frontend/core separation, and Lua-vs-Rust responsibility.
5. **Maintainability:** dead code, duplication, overly coupled modules, hidden conventions, stale comments/config, and unnecessary complexity.
6. **Performance:** blocking work, cloning/allocation, lock scope, unbounded data, hot rendering paths, and avoidable I/O.
7. **Tests:** meaningful behavior coverage, negative cases, race/cancellation coverage, brittle fixtures, and assertions that can pass for the wrong reason.

For each finding, record:

- Severity: `Critical`, `High`, `Medium`, or `Low`
- Exact `file:line` evidence and affected behavior
- Reproduction or failing scenario
- Root cause, not only the symptom
- Smallest recommended fix
- Existing test coverage and the regression test needed
- Confidence: `verified` or `needs confirmation`

Do not count style preferences as findings unless they create a concrete maintenance or correctness risk.

## Review sections

### 1. Workspace architecture, protocol, entry points, and distribution
**Status: finished** — see `CODEBASE_AUDIT_SECTION1.md` (cleanup applied).

**Purpose:** Establish the system boundaries and verify that public/wire contracts, startup paths, and shipped artifacts agree before reviewing implementations.

**Files:**

- `Cargo.toml`, crate `Cargo.toml` files, `.github/workflows/`
- `protocol/src/**` and protocol tests
- `core/src/lib.rs`, `core/src/run.rs`, `core/src/agent.rs`, `core/src/chat.rs`
- `tui/src/lib.rs`, startup/CLI portions of `tui/src/main.rs`
- `npm/**`, `tmux-super-popup/**`
- User-facing architectural claims in tracked Markdown/config documentation

**Focus:**

- Dependency direction: client → protocol → core
- Serialization compatibility, defaults, enum evolution, and bounded payloads
- CLI modes, daemon/local startup, shutdown, panic reporting, and platform branches
- Feature flags and differences between headless and TUI builds
- Release versioning, package contents, installer behavior, and stale documentation

**Exit condition:** A concise architecture/data-flow map and a list of contracts later sections must preserve.

---

### 2. Runtime, session orchestration, and RPC

**Purpose:** Audit the headless control plane from an incoming command through a completed or cancelled turn.

**Files:**

- `core/src/runtime/**`
- `core/src/rpc/**`
- `core/src/session_sink.rs`
- Closely related unit tests and `core/tests/driver_turn_test.rs`, `rpc_daemon_test.rs`, `interactive_esc_test.rs`, `stream_tools_test.rs`

**Focus:**

- Driver state machine, event ordering, retries, cancellation, and cleanup
- Session isolation, ownership, reconnect/disconnect behavior, and concurrent access
- RPC framing, malformed/unbounded input, backpressure, and compatibility with protocol types
- Approval/key reply lifecycle and orphaned waiters
- Lock scope, transcript cloning, task panic handling, and daemon shutdown

**Exit condition:** Every command/event path is traced, with terminal states and resource cleanup verified.

---

### 3. LLM providers, streaming, prompts, and token accounting

**Purpose:** Verify that each provider implements the same semantic contract despite different wire formats.

**Files:**

- `core/src/llm/**`
- Provider unit tests and `core/tests/codex_provider_test.rs`, `openai_compat_test.rs`, `mock_provider_injection_test.rs`, `think_parser_test.rs`, `token_tracker_test.rs`

**Focus:**

- Request construction, roles, tool definitions/results, images, and prompt assembly
- SSE parsing across chunk boundaries; partial tool-call accumulation
- HTTP status/error classification, retries, rate limits, and cancellation
- Authentication/token refresh, secret redaction, URL/host decisions, and cache headers
- Usage consistency and context/token estimates across providers
- Duplicated provider logic that can drift semantically

**Exit condition:** A provider parity matrix covering supported inputs, outputs, errors, usage, and cancellation.

---

### 4. Tools, approval policy, filesystem, shell, and processes

**Purpose:** Audit the highest-risk execution boundary where model output becomes host-side effects.

**Files:**

- `core/src/tools/**`
- `core/src/processes.rs`, `core/src/shell_split.rs`
- `core/default-command-policy.yaml`
- Matching unit/integration tests, especially approval, command policy, shell, read/write/edit, snapshot, and tool-argument tests

**Focus:**

- Approval as an invariant across every invocation path
- Command parsing/classification versus actual shell semantics
- Path normalization, working-directory containment, symlinks, TOCTOU, and atomic writes
- Cancellation, process groups, output limits, timeouts, environment inheritance, and owner isolation
- Snapshot/rollback correctness and consistency between live/non-live execution paths
- Registry enablement, dynamic safety metadata, schema validation, and error visibility

**Exit condition:** A complete matrix of each tool’s side effects, approval class, containment rules, cancellation, and rollback behavior.

---

### 5. Lua engine, extension loading, and registration lifecycle

**Purpose:** Review the scripting boundary itself: what Lua can load, register, persist, and execute.

**Files:**

- `core/build.rs`
- `core/src/ext/engine.rs`, `loader.rs`, `lua_tool.rs`, `types.rs`, `mod.rs`
- `core/src/ext/catalog.rs`, `snapshots.rs`, `ops_commands.rs`, `ops_events.rs`, `ops_plugins.rs`, `ops_tools.rs`
- Corresponding `ext/*_tests.rs` and extension/catalog/Lua-tool integration tests

**Focus:**

- VM sandbox and exposed standard libraries
- Init/plugin loading order, path trust, remote catalog handling, and failure semantics
- Registration/override rules for tools, commands, events, and plugins
- Lua↔Rust conversion, schema enforcement, callback lifetime, and thread-safety assumptions
- Seed/refresh behavior and protection of user-customized files
- Build-time embedding and drift between embedded and seeded content

**Exit condition:** A lifecycle diagram from bundled source or user plugin to registered runtime object, including all trust transitions.

---

### 6. Lua host APIs, async jobs, shared state, and bundled Lua behavior

**Purpose:** Audit what extensions can do after loading and verify that bundled Lua uses those capabilities safely.

**Files:**

- `core/src/ext/api.rs`, `api_ui.rs`, `ctx.rs`, `inbox.rs`, `jobs.rs`
- `core/defaults/lua/**`
- API/context/job/inbox tests plus `compact_test.rs`, `history_menu_test.rs`, `tasklist_pane_test.rs`, `lua_api_test.rs`, `lua_tool_nested_test.rs`

**Focus:**

- Capability boundaries for shell, database, UI, agent spawning, filesystem, and network access
- Whether APIs route through canonical policy, approval, cancellation, and ownership paths
- Async job lifecycle, synchronization, callback errors, shared state, and leaks
- UI interaction semantics and frontend independence
- Bundled command/tool correctness, persistence assumptions, and stale compatibility migrations
- Silent Lua failures and whether users receive actionable errors

**Exit condition:** A capability table for every exposed `ctx`/API operation, including policy and lifecycle guarantees.

---

### 7. Configuration, persistence, history, and migrations

**Purpose:** Verify durable state from defaults and provider settings through conversation storage and migration.

**Files:**

- `core/src/config/**`, `core/src/config/pages/**`
- `core/src/session_db.rs`, `session_db_tests.rs`
- `core/src/commands.rs`, `pane_content.rs`, `update_check.rs`, `util.rs`
- `core/defaults/AGENTS.md` and relevant default YAML
- Related config, session DB/sink, compact, and remote-config tests

**Focus:**

- Precedence and consistency among defaults, files, environment, CLI, and runtime updates
- YAML validation, unknown/corrupt values, secret handling, and atomic persistence
- Migration idempotence, partial failure, backup/recovery, and data-loss risks
- SQL parameterization, transaction boundaries, conversation isolation, and retention
- History/compaction semantics, usage aggregation, timestamps, and large-database behavior
- Update checks and other network/file failures that should not silently alter behavior

**Exit condition:** A data ownership map and tested migration/recovery path for every persisted format.

---

### 8. TUI application state, input, commands, and terminal lifecycle

**Purpose:** Audit the client-side state machine and all user-input paths independently of rendering details.

**Files:**

- Remaining orchestration in `tui/src/main.rs`
- `tui/src/ui/app/**`
- `tui/src/ui/input.rs`, `commands/**`, `autocomplete.rs`, `catalog.rs`, `picker.rs`, `prompt.rs`, `setup.rs`
- Associated unit tests and input/integration/subagent tests

**Focus:**

- Local versus socket connection parity and reconnect behavior
- Event-to-state transitions, stale responses, queued input, and focus/mode ownership
- Editor Unicode behavior, paste handling, keymaps, autocomplete, and command dispatch
- Approval and interactive prompt flows under cancellation/disconnect
- Terminal raw mode, alternate screen, signals, panic cleanup, and clipboard/platform behavior
- Whether the TUI duplicates or reaches into state that should remain daemon-owned

**Exit condition:** A state-transition map for normal turns, prompts, cancellation, disconnect, and shutdown.

---

### 9. TUI rendering, panes, transcript, and terminal compatibility

**Purpose:** Review presentation as a deterministic projection of protocol/view state and test difficult terminal dimensions/content.

**Files:**

- `tui/src/ui/render/**`
- `tui/src/ui/*pane*.rs`, `fullscreen.rs`, `stats.rs`, `theme.rs`, `tool_display.rs`, `transcript_view.rs`, `color.rs`
- Rendering/backend/wrap/messages/tool-display/diff-preview/Unicode tests

**Focus:**

- Chronological ordering, incremental rendering, scroll/viewport invariants, and resize behavior
- Unicode width, wrapping, markdown, code blocks, diffs, images, and malformed content
- Pane layout at zero/tiny dimensions; clipping and arithmetic underflow/overflow
- Terminal capability differences, scroll regions, backend output, and cleanup
- Render-loop allocations, repeated parsing/cloning, and unnecessary full redraws
- Snapshot/golden tests that assert semantics rather than incidental formatting

**Exit condition:** A rendering invariant checklist exercised across narrow, wide, Unicode, streaming, and pane-heavy cases.

## Recommended order and batching

Review in the numbered order. Sections 1–4 establish contracts and high-risk execution paths. Sections 5–7 cover customization and durable state built on those contracts. Sections 8–9 then verify that the TUI remains a client of the audited core/protocol behavior.

Treat each section as one review batch:

1. Read production files and their adjacent tests.
2. Trace public entry points to side effects and terminal states.
3. Run the narrowest existing tests for that section.
4. Reproduce suspected findings before recording them.
5. Write the section report before moving on.
6. Maintain a cross-section list for issues whose root cause belongs elsewhere; report each issue once at its owner.

## Per-section report template

```markdown
# Section N — <name>

## Scope and checks
- Files reviewed:
- Tests/commands run:
- Unreviewed or uncertain areas:

## Architecture and invariants
- Entry points:
- Owned state:
- Trust boundaries:
- Required invariants:

## Findings
### [Severity] Short title
- Evidence: `path/file.rs:line`
- Scenario:
- Root cause:
- Impact:
- Recommended fix:
- Regression test:
- Confidence: verified | needs confirmation

## Coverage gaps
- ...

## Clean areas verified
- ...
```

## Final synthesis

After all nine sections:

- Deduplicate findings by root cause.
- Re-check line references against the final working tree.
- Rank fixes by severity, exploitability, user impact, and implementation risk.
- Separate verified defects from design recommendations and coverage gaps.
- Produce a short dependency-aware remediation sequence.
- Run workspace-wide `cargo fmt --check`, `cargo check`, and `cargo test`; add platform-specific or Lua syntax checks where the reviewed behavior requires them.
