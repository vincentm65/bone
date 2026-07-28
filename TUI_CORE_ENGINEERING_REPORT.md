# TUI and Core Engineering Audit

Date: 2026-07-27

Scope: `tui/`, `core/`, and the `protocol/` boundary between them. Build, test,
dependency, and CI observations are included where they affect safe refactoring.

Goal: reduce accidental complexity, coupling, build/test weight, and runtime
risk without removing user-visible features.

## Executive summary

Bone does not need a rewrite. It already has a broad and unusually strong test
suite, a shared runtime protocol, and clear intent that the daemon owns
authoritative state. The safest route is to consolidate responsibilities behind
compatibility adapters, fix three existing correctness risks, and then shrink
the large modules one behavior-preserving slice at a time.

The most important findings are:

1. Persisted conversations do not retain all model-facing replay data.
   Reasoning metadata is absent from ordinary message rows, and ordered output
   is explicitly skipped during serialization. Reloading a conversation can
   therefore change what a provider sees.
2. `bone.submit` uses one process-global, unscoped queue. With multiple managed
   conversation actors, whichever actor polls first can consume another
   conversation's submitted prompt.
3. Correctness-critical runtime events share a lossy broadcast channel with
   high-volume deltas. The TUI waits on `TurnComplete` and `CommandComplete`,
   but lag recovery only refreshes process state.
4. The TUI interprets runtime events in three separate pumps. This duplicates
   policy, permits behavior drift, and forces several methods to drain and wait
   on the same receiver.
5. Core's `Driver`, RPC daemon, tool context, configuration service, and Lua
   context each combine several distinct responsibilities. Their size reflects
   real ownership and lifecycle coupling, not just a need for more files.
6. There are several safe, measurable debloat wins: unused dependencies,
   duplicate PNG versions, excess `syntect` features, repeated integration-test
   linking, a redundant build-script generator, and production-dead prompt
   rendering compatibility code.

Recommended strategy:

- Fix conversation isolation, durable replay, and event reliability first.
- Introduce a single TUI reducer and a request-correlated client layer.
- Keep an `App` facade while grouping state and moving effects behind ports.
- Make the core turn produce one typed journal, then split turn orchestration
  from streaming, tools, and persistence.
- Replace recursive tool/context ownership with an immutable catalog,
  session-owned runtime state, and a lightweight execution context.
- Keep legacy configuration and Lua shapes as ingress adapters until trace and
  fixture tests prove equivalent behavior.

## Remediation status

The high-risk findings and the safe debloat work identified by this audit have
now been implemented. The source references in the evidence sections below
describe the pre-remediation baseline.

Completed:

- Schema v10 stores a versioned, complete model-facing message payload while
  preserving normalized columns, legacy rows, and legacy checkpoints. Reasoning
  metadata and exact output ordering now survive restart/reload.
- `bone.submit` and `ctx.conversation.submit` use one bounded FIFO per Lua
  runtime. Extension reloads retain queued prompts, and deterministic tests
  prove two daemon actors cannot consume each other's work.
- Socket lag is explicit. Correlated synchronization can repair authoritative
  session state and the full transcript, and pending approval/key gates are
  replayed before the repair response.
- Prompt turns, slash commands, keymaps, and state repairs carry request IDs.
  Current daemons use exact matching; a startup probe provides a bounded legacy
  fallback instead of accepting ambiguous uncorrelated replies unconditionally.
- Commands arriving while a turn or interactive command is active are held in
  one daemon-owned FIFO and serviced when idle. They are no longer silently
  dropped. Remote EOF now closes frontend event receivers.
- The TUI deduplicates replayed approval/key gates, adopts concurrent turns
  encountered by blocking waiters, and turns keymap lag into a recoverable
  notice instead of terminating the application.
- Inline and fullscreen terminal ownership now use cleanup guards. Editor
  handoff uses unique automatically deleted temporary files.
- Repeated terminal-width publications and idle process-view redraws were
  removed; input-history byte accounting and stats heat colors are cached.
- Render and approval call sites now pass small typed option objects instead of
  long positional argument lists.
- The duplicate PNG version, unused TUI dependencies, excess `syntect` features,
  and empty core build dependencies were removed. The resolved lockfile is 11
  packages smaller.
- The build script shares one Lua-source collector, and CI now enforces
  formatting, all-target compilation, strict Clippy, full/default-minimal Rust
  tests, web tests, and macOS/Windows compilation.
- The existing public `SessionSink::append_message` implementation contract is
  retained; a typed `append_chat_message` path adds lossless built-in
  persistence without forcing external sink implementations to change.

Post-remediation validation on the current tree:

- `cargo fmt --all -- --check`: passed.
- `cargo check --workspace --all-targets --all-features`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  passed.
- `cargo test --workspace --all-features`: 817 passed; one doctest ignored.
- `cargo test -p bone-core --no-default-features`: passed.
- `node --test webui/tests/*.test.mjs`: two passed.
- `cargo build --release`: passed.
- The lockfile contains 321 packages, down from 332. The stripped release
  binary is 15,905,416 bytes, up 127,648 bytes (0.8%) from the audit baseline:
  the dependency graph is smaller, but the reliability and recovery machinery
  has a modest code-size cost. This report does not claim a binary-size win.

Deliberately deferred:

- A single TUI event-owner/reducer, cohesive `App` state groups, a typed core
  turn journal, tool/context ownership cleanup, RPC policy extraction, and
  canonical live configuration remain staged architectural work. Combining
  those into this correctness patch would increase regression risk.
- The v10 replay payload favors correctness and simple legacy fallback over
  minimum database bytes: visible message/image/tool data exists in both the
  normalized projection and versioned payload. A supplemental-only payload can
  be considered later with fixture-based migration tests.
- Rust struct-literal construction of protocol enums changed where correlation
  fields were added. JSON compatibility is retained with serde defaults; if
  the Rust protocol types are a promised external API, the next release should
  document this boundary or introduce constructor-only public usage.

## Baseline and audit evidence

Physical line counts include inline unit tests:

| Area | Current baseline |
|---|---:|
| `core/src` | 37,609 lines |
| `tui/src` | 19,350 lines |
| `protocol/src` | 1,641 lines |
| `core/tests` + `tui/tests` | 12,263 lines |
| Largest TUI module | `tui/src/ui/app/mod.rs`, 3,449 lines |
| Largest core module | `core/src/ext/ctx.rs`, 3,100 lines |
| Mutable fields on `App` | 52 |
| Rust tests discovered with all features | 793 |
| Release binary, stripped Linux x86-64 | 15,777,768 bytes |
| Cold release build observed | about 1 minute 45 seconds |
| Resolved third-party packages on Linux | 258 |
| Integration-test executables | 37, currently about 5.15 GB total |

Validation run during the audit:

- `cargo fmt --all -- --check`: passed.
- `cargo check --workspace --all-targets --all-features`: passed.
- `cargo test --workspace --all-features`: 792 passed; one doctest ignored.
- `cargo test -p bone-core --no-default-features`: passed.
- `node --test webui/tests/*.test.mjs`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  failed on nine existing core diagnostics. Non-fatal Clippy reports additional
  warning locations in core, TUI, and tests.

The release profile already uses thin LTO, stripping, and one codegen unit
(`Cargo.toml:9-12`). Binary-size work should focus on dependency features and
architecture before making more aggressive profile changes.

## Constraints for feature-preserving work

Treat these as non-negotiable compatibility surfaces:

- TUI, headless, daemon, remote attach, and web modes.
- All current providers and provider-specific replay semantics.
- Safe/danger approval behavior and mid-turn interaction.
- Background processes, sub-agents, steering, queueing, and cancellation.
- Lua tools, commands, hooks, panes, settings, keymaps, and existing return
  shapes.
- Existing configuration and SQLite migrations.
- Linux, macOS, Windows, and Android-specific compilation behavior already
  represented in the manifests.

Do not optimize only for fewer lines. A file split that leaves the same object
owning the same 52 fields and the same three event loops is not meaningful
debloating. The useful reductions come from one owner per state transition,
typed boundaries, fewer live representations, and fewer linked copies of the
same code.

## Priority map

| Priority | Finding | User risk | Effort | Refactor risk |
|---|---|---|---|---|
| P0 | Preserve complete model-facing messages in SQLite | Context/replay changes after reload | M | M |
| P0 | Scope `bone.submit` to one runtime/conversation | Cross-conversation prompt routing | S-M | L-M |
| P0 | Make completion/control delivery reliable | Partial UI state or a stuck event pump | M-L | M |
| P1 | Add request IDs and one TUI event owner | Wrong response satisfies a waiter | M-L | M |
| P1 | Make terminal lifecycle exception-safe | Shell left in raw/altered state | S-M | L |
| P1 | Replace recursive tool/context ownership | Clone cost and fragile lifecycle rules | L | M |
| P1 | Introduce a typed turn journal | Divergent headless/daemon persistence | M | M |
| P1 | Split runtime policy from RPC transport | Command behavior drifts by phase | L | M |
| P1 | Establish one canonical live configuration | Schema and compatibility drift | L | H |
| P2 | Group TUI state and input policy | Ongoing feature cost and regressions | M-L | M |
| P2 | Split Lua/DB capability modules | Comprehension and test isolation | M | L-M |
| P2 | Trim dependencies/build/test artifacts | Build time, disk use, binary size | S-M | L |

## P0: correctness work before structural cleanup

### 1. Persist the complete model-facing message

Evidence:

- `protocol/src/message.rs:103-119` defines reasoning, encrypted reasoning
  items, and ordered `output_sequence`; `output_sequence` has `#[serde(skip)]`.
- `core/src/runtime/driver.rs:587-688` deliberately records the exact order of
  reasoning, text, and tool calls.
- `core/src/llm/providers/codex.rs:260` documents that Responses replay ordering
  matters.
- `core/src/session_db.rs:104-117` has no reasoning or ordered-output fields in
  `StoredMessage`.
- `core/src/session_db.rs:119-141` reconstructs every ordinary database message
  with `reasoning: None`, empty reasoning items, and an empty output sequence.

This is more than display history. Reloading or restarting can produce a
different provider request from the request that would have been produced
without the reload. Checkpoints preserve some serialized reasoning, but still
lose ordered output because it is skipped.

Recommendation:

- Add a versioned `PersistedMessageV2` payload containing every model-facing
  field, including a serializable ordered output representation.
- Keep the current normalized columns for search, listing, and compatibility.
- Add a nullable `payload_json` column through the next schema migration.
- Read V2 when present and fall back to the current column mapper for existing
  rows.
- Make database round-trip tests compare the complete provider request payload,
  not only message role and visible text.

Feature guard: load old database fixtures and prove that search/history output
is unchanged while new rows preserve all provider fields.

### 2. Make `bone.submit` runtime-scoped

Evidence:

- `core/src/ext/inbox.rs:12-17` creates one process-global
  `OnceLock<Mutex<VecDeque<String>>>`.
- Each managed daemon actor polls `next_background_prompt` every 200 ms in
  `core/src/rpc/mod.rs:2034-2074`.
- `next_background_prompt` calls the unscoped global `pop` at
  `core/src/rpc/mod.rs:970-981`.
- Jobs and processes already use explicit conversation scopes, so this queue is
  the outlier.

With two active conversation actors, the actor that wakes first owns the pop,
regardless of which Lua VM called `bone.submit`. Besides incorrect behavior,
this can disclose submitted content across conversations.

Recommendation:

- Put the submit queue in runtime-owned state captured by the corresponding
  `ExtensionManager`/Lua VM.
- Give `DaemonCtx` that queue directly instead of consulting a global.
- Use a typed conversation/session ID for any routing that crosses a runtime
  boundary.
- Retain the current bounded FIFO behavior per runtime.

Feature guard: a deterministic two-actor test should submit unique prompts from
both actors and assert exact, ordered, non-crossing delivery.

### 3. Separate reliable control from coalescible event data

Evidence:

- `core/src/rpc/mod.rs:115-126` creates a 1,024-item broadcast event buffer and
  an unbounded command queue.
- Both socket paths continue silently on broadcast lag at
  `core/src/rpc/mod.rs:442-443` and `core/src/rpc/mod.rs:585-586`.
- The TUI turn pump waits for `TurnComplete` at
  `tui/src/ui/app/stream/mod.rs:432-438`.
- The command pump waits for `CommandComplete` at
  `tui/src/ui/app/stream/mod.rs:618-654`.
- TUI lag recovery only sends `GetProcesses`
  (`tui/src/ui/app/mod.rs:1854-1858`).

Text deltas may be coalesced or repaired from an authoritative transcript.
Approval requests, configuration results, completion events, and request
acknowledgements cannot be silently discarded. The current channel does not
distinguish them.

Recommendation:

- Put `request_id`, `turn_id`, and monotonic sequence information on protocol
  envelopes.
- Deliver acknowledgements and phase completion over a reliable, bounded path.
- Keep broadcast for replaceable snapshots and streaming telemetry.
- Define explicit lag behavior: request a full turn/session snapshot or
  disconnect an irrecoverably slow client. Never silently continue with unknown
  state.
- Bound command ingress per client and define queue-full behavior. Preserve
  interactive priority for cancel, approval, and key replies.

Feature guard: inject lag immediately before every control event and assert
that the client either completes with a reconciled transcript or receives a
clear disconnection error.

## TUI engineering findings

### 4. Use one event consumer and one pure reducer

Runtime events are interpreted independently in:

- idle mode: `tui/src/ui/app/mod.rs:704-783`;
- turn mode: `tui/src/ui/app/stream/mod.rs:357-515`;
- live-command mode: `tui/src/ui/app/stream/mod.rs:527-743`;
- turn event application:
  `tui/src/ui/app/stream/mod.rs:748-974`.

Policy has already drifted. Status and notice events are surfaced differently
by phase, and `FrontendState` is applied while idle but ignored during a turn.
Several request helpers also drain the same receiver while waiting for one
specific event (`tui/src/ui/app/mod.rs:1098-1205`).

Target shape:

```text
transport task ──> UiEvent queue ──> update(AppModel, UiEvent)
                                      │
                                      └──> Vec<Effect>

UiPhase = Idle | Turn(turn_id) | Command(request_id) | Approval(request_id)
```

Only the transport task should read runtime events. The pure reducer should
apply common snapshots, configuration, processes, panes, notices, and
disconnects once. Phase-specific code decides only what an acknowledgement
completes and which input policy is active. An effect executor owns terminal
I/O, process launches, clipboard work, and protocol sends.

This is the highest-value TUI change because it removes duplicate behavior
rather than redistributing it.

### 5. Correlate every request that waits for a response

`send_and_await_snapshot` drains the receiver, sends a command, and accepts the
first later `StateSnapshot` (`tui/src/ui/app/mod.rs:1118-1150`). Keymap dispatch
uses the same first-matching-event pattern
(`tui/src/ui/app/keymap.rs:34-57`). In a multi-client daemon, another client's
mutation can publish a matching event between those operations.

Add a request envelope:

```rust
struct RequestId(u64);

struct RuntimeRequest {
    id: RequestId,
    command: RuntimeCommand,
}

struct RuntimeReply {
    request_id: RequestId,
    result: Result<ReplyPayload, ProtocolError>,
}
```

Conversation load already correlates by conversation ID; configuration
mutations already have optional request IDs. Generalize that safe pattern.
Pending replies belong in the client layer, not in UI methods that temporarily
take over the event receiver.

### 6. Turn `App` into a facade over cohesive state

`App` owns 52 fields at `tui/src/ui/app/mod.rs:444-564`, including transport,
database access, configuration, composer input, queues, turn timing, approvals,
rendering, panes, jobs, processes, shell rows, and terminal background state.
`stream/mod.rs` remains an `impl App`, so it is a file split rather than an
ownership boundary.

First group state without changing behavior:

```text
App
├── client: FrontendClient
├── composer: ComposerState
├── turn: TurnState
├── panes: PaneState
├── background: BackgroundState
├── config: ConfigClientState
└── terminal: TerminalState
```

Then let the reducer update these groups. Keep `App` as the public facade while
tests and callers migrate. Avoid a big-bang ECS or framework rewrite.

### 7. Unify keyboard policy

Idle keys are handled at `tui/src/ui/app/mod.rs:2369-2577`. Streaming and
live-command input use a separate 257-line, 15-argument helper at
`tui/src/ui/app/stream/mod.rs:1240-1496`, with a Clippy exemption. Pane and queue
policy appears in both paths.

Use:

```text
reduce_key(InputContext, KeyEvent) -> Vec<UiAction>
```

`InputContext` explicitly describes phase, key ownership, approval, queue,
active pane, and autocomplete. Intentional differences remain visible policy:
for example, Enter can submit while idle and enqueue while streaming. The
effect executor performs resulting sends, editor launches, redraws, or local
shell execution.

Add a phase-by-key matrix before replacing the existing handlers.

### 8. Make terminal ownership exception-safe

`App::run` installs a panic hook and then uses `?` throughout the event loop
(`tui/src/ui/app/mod.rs:1368-1529`). Cleanup runs only on the normal path.
Ordinary read, draw, resize, or insertion errors can therefore return without
restoring raw mode, paste mode, colors, or the viewport. Fullscreen handling has
the same manual lifecycle (`tui/src/ui/fullscreen.rs:45-67`).

Introduce:

- `TerminalSession`, which owns raw mode, bracketed paste, viewport, and
  background changes;
- `AlternateScreenGuard` for fullscreen views;
- best-effort `Drop` restoration plus explicit `finish()` that can report
  cleanup errors;
- fake-terminal tests that inject errors and panics at every lifecycle step.

This is a small change with direct user-facing reliability value.

### 9. Stop collapsing actionable errors

`run_remote_command` returns `Option<()>`
(`tui/src/ui/app/stream/mod.rs:527`) and ignores many command or terminal
results with `.ok()` through line 740. Input polling converts errors into
either no input or a synthetic null key
(`tui/src/ui/app/stream/mod.rs:1260-1270`).

Return an explicit result:

```text
Result<CommandOutcome, UiError>
CommandOutcome = Completed | Cancelled | Disconnected
```

Mark effects as required or best effort. A closed runtime command channel is
required and should preserve the user's draft; a redraw during shutdown may be
best effort. This distinction removes repeated ad hoc swallowing while keeping
shutdown robust.

### 10. Enforce the frontend/core boundary

The TUI library broadly reexports core modules
(`tui/src/lib.rs:11-14`), which hides direct coupling. The app also:

- reads the process-global job registry
  (`tui/src/ui/app/mod.rs:1863-1989`);
- opens the local SQLite database, including when attached remotely
  (`tui/src/ui/app/mod.rs:573-602`, `3235-3264`);
- directly executes `ShellTool` for `:command`
  (`tui/src/ui/app/stream/mod.rs:1206-1238`).

This creates local/remote divergence. In particular, a remote TUI has no
protocol job snapshot and displays statistics from the client's local database.

Introduce a `FrontendPort` with explicit operations for:

- runtime requests and event subscription;
- scoped job/process snapshots;
- usage-stat queries;
- catalog/reload operations;
- explicitly local shell execution.

The TUI library should eventually depend on protocol and presentation DTOs.
The `bone` binary/bootstrap adapter can depend on core to run an embedded
daemon. Preserve `:command` semantics, but name and test whether it is
client-local or daemon/project-local rather than letting dependency placement
decide accidentally.

Also move the display-only `Message` and `ToolDisplay` types out of
`core/src/chat.rs:28-96` into the TUI.

### 11. Simplify bottom-pane rendering around one layout model

`draw_bottom_pane_with_tick` is roughly 460 lines starting at
`tui/src/ui/render/bottom_pane.rs:581`. It supports an older direct `Prompt`
branch, but production callers pass `None`
(`tui/src/ui/app/mod.rs:2194`, `tui/src/ui/render/mod.rs:446`). Only tests use
the compatibility branch.

Adapt `Prompt` to `PanePage` at one boundary, then split the renderer into:

- `RunningStrip`;
- `Composer`;
- `Autocomplete`;
- `PageRegion`;
- `StatusBar`.

A shared `BottomPaneLayout` must drive both desired-height calculation and
drawing. That prevents the common failure mode where “cleanup” makes sizing and
rendering disagree. After compatibility tests move to the adapter, the dead
branch can likely remove 200-300 lines without removing prompt behavior.

### 12. Move tool presentation to a typed protocol shape

`tui/src/ui/tool_display.rs` is 808 lines of tool-specific parsing for shell,
file, browser, Firefox, sub-agent, and generic results. Shell presentation adds
another lexer in `tui/src/ui/render/messages.rs:481-734`. Inline shell failure
is inferred from display strings at `tui/src/ui/app/stream/mod.rs:1215`.

Have the daemon/tool host emit a typed `ToolPresentation`:

```text
label, severity, kind, structured_content, expansion_policy, image metadata
```

Put `ToolDisplayConfig` in `protocol` as a typed field instead of sending an
opaque JSON map (`protocol/src/event.rs:110-121`). Keep the current generic
client parser as a version-compatibility fallback. This centralizes provider/
tool knowledge while preserving rendering for old daemons.

### 13. Low-risk TUI improvements

These can land independently after characterization tests:

- Use a securely created unique tempfile instead of the shared
  `bone-edit.txt` path (`tui/src/ui/app/editor.rs:10-26`).
- Send terminal width only when it changes; it is currently sent during every
  viewport reconciliation (`tui/src/ui/app/mod.rs:1538-1545`).
- Redraw/re-wrap the process viewer only after data, resize, or key changes
  instead of rebuilding up to 64 KiB every 100 ms
  (`tui/src/ui/process_view.rs:35-84`).
- Cache status/spinner resolution currently rebuilt on the 90 ms render cadence
  (`tui/src/ui/app/mod.rs:2122-2181`).
- Cache the 15-color heat gradient instead of allocating it per cell
  (`tui/src/ui/stats.rs:1009-1051`).
- Maintain input-history byte size and use `VecDeque` instead of repeatedly
  summing every entry and removing index zero
  (`tui/src/ui/input.rs:340-352`).
- Add shared display-width/grapheme helpers. Current cursor state counts Unicode
  scalar values, which is panic-safe but can split a user-perceived grapheme.
- Move clipboard conversion, editor handoff, catalog synchronization, and tmux
  calls behind the effect executor or an appropriate blocking task.

## Core engineering findings

### 14. Make the turn loop an explicit transaction

`Driver` has roughly twenty public concerns
(`core/src/runtime/driver.rs:75-115`). Its primary function runs from about line
222 to line 1,050 and owns hooks, context estimates, request retries, stream
decoding, transcript mutation, tool execution, usage, persistence, cancellation,
and event emission.

Keep one readable orchestration loop, but extract stateful phases:

```text
Driver
├── TurnState
├── RequestRunner
├── StreamAccumulator
├── ToolRound
└── TurnJournal
```

Make fields private and construct the driver from `TurnInput` plus
`TurnServices`. `StreamAccumulator` should own ordered output assembly and usage
capture. `ToolRound` should own approval, execution, state updates, and repeated
failure detection. The orchestration loop remains the place where phase order
is obvious.

Large histories and transcripts are cloned repeatedly at
`core/src/runtime/driver.rs:373`, `430`, and `545`. After ownership is clear,
use immutable shared snapshots or lazy context access where profiling shows
value. Do not optimize clones before replay semantics are locked down.

### 15. Produce one typed `TurnJournal`

`SessionSink::append_message` takes eight loosely related primitive parameters
(`core/src/session_sink.rs:27-42`). The driver writes through that sink while
also building `persist_messages`. The daemon uses a `NullSessionSink` and later
persists through `RuntimeSession::apply_outcome`
(`core/src/rpc/mod.rs:1851`, `core/src/runtime/session.rs:344-399`), while
headless mode writes incrementally through `SessionWriter`
(`core/src/agent.rs:16-198`).

Replace those parallel representations with:

```text
TurnJournal
├── Vec<PersistedMessageV2>
├── Vec<UsageRecord>
└── optional context checkpoint
```

One repository transaction API should consume the journal in daemon and
headless modes. If incremental headless durability is required, make it an
explicit journal strategy using the same records. This also provides the right
place to fix complete message replay.

### 16. Break the `ToolHandler`/`AppCtxState` ownership cycle

`AppCtxState` boxes a full `ToolHandler` to break a type cycle
(`core/src/ext/ctx.rs:200-218`), then `apply_to` clones it
(`core/src/ext/ctx.rs:252-267`). `ToolHandler` stores an `AppCtxState` back
pointer (`core/src/tools/registry.rs:192-212`) and passes both cloned objects
through an 11-argument call (`core/src/tools/registry.rs:49-105`,
`494-518`). The driver manually clears `app_state` before rebuilding it to
avoid recursive chains (`core/src/runtime/driver.rs:854-858`).

Replace this with:

```text
ToolCatalog        immutable definitions/implementations, behind Arc
ToolRuntimeState   enabled set, snapshots, host state, owner, cancellation
ExecutionServices  approval, events, nested-agent/tool services
CallContext        call ID, depth, working directory, session ID
```

The handler must not contain a context that contains another handler. This
reduces clone cost, eliminates manual cycle-breaking, and turns Clippy's
too-many-argument warnings into a useful typed boundary.

### 17. Split runtime policy from RPC transport

`core/src/rpc/mod.rs` is 2,095 lines and combines broadcast hubs, socket
transport, remote clients, session management, daemon state, configuration,
Lua commands, lifecycle, processes, and turn pumping. `handle_idle_command`
alone spans `core/src/rpc/mod.rs:1201-1822`.

Command policy is repeated in local connection handling, interactive command
pumps, idle dispatch, and active-turn dispatch
(`core/src/runtime/conn.rs:126-181`, `core/src/rpc/mod.rs:1095-1145`,
`1900-1949`).

Target modules:

```text
rpc/
├── codec
├── transport
├── client
└── session_manager

runtime/
├── actor
├── command_policy
├── conversation_commands
├── config_commands
├── process_commands
└── turn_pump
```

A central `CommandPolicy` classifies each variant as idle-only, busy-safe,
interactive reply, queued, or rejected. Domain handlers own mutations. The
transport only frames and routes typed envelopes.

### 18. Use one awaitable cancellation tree

Cancellation is polled independently at 25 ms in the driver
(`core/src/runtime/driver.rs:245-265`), 50 ms in extension agents
(`core/src/ext/ctx.rs:2909-2918`), and 100 ms in jobs
(`core/src/ext/jobs.rs:418-424`). Dropping a `spawn_blocking` join handle
detaches the task; it does not stop Lua work
(`core/src/runtime/driver.rs:456-475`, `core/src/rpc/mod.rs:1122-1138`).

Introduce one awaitable cancellation token with child tokens and a runtime-owned
task group. A non-cooperative Lua VM call should either be serialized through a
Lua actor and awaited or reported as still stopping; it should not continue
mutating shared state after the caller is told cancellation completed.

Preserve the current fast keyboard response and add tests for cancellation
during provider connect, retry backoff, Lua hook, tool execution, sub-agent
wait, approval, and key input.

### 19. Keep one canonical live configuration

`ConfigStore::Inner` owns typed settings/domains and a live legacy
`CustomConfigs` mirror (`core/src/config/store.rs:18-28`). Every mutation pushes
both representations into extension runtimes
(`core/src/config/store.rs:137-148`). Schema metadata is manually rebuilt in
`schema_for` (`core/src/config/store.rs:270-565`), while key routing and
validation are separately encoded in `core/src/config/settings.rs:690-1074`.

Recommendation:

- Make one immutable `ResolvedConfig` the live authority.
- Derive protocol schema, CLI/TUI field metadata, validation, defaults, and key
  routing from declarative field descriptors.
- Keep legacy YAML/page parsing as a one-way ingress adapter.
- Keep a Lua compatibility view that projects from `ResolvedConfig`; do not
  maintain it as another mutable authority.
- Serialize writes through a config actor, or use a two-phase
  revision-checked commit so filesystem I/O and extension validation do not
  hold the global config mutex (`core/src/config/store.rs:606-684`).

This should be one of the last large migrations because configuration
compatibility is a feature. Start by generating metadata for one namespace and
compare old/new snapshots byte-for-byte.

### 20. Split Lua context by capability without changing the Lua API

`core/src/ext/ctx.rs` is 3,100 lines. `CtxConfig` has around twenty optional or
required fields (`core/src/ext/ctx.rs:120-158`), and one builder installs
filesystem, I/O, UI, usage, conversation, tools, sessions, database, settings,
configuration, and agent APIs.

Mechanically extract capability modules implementing a small installer
interface:

```text
ctx/
├── base
├── fs
├── ui
├── usage
├── conversation
├── tools
├── session
├── db
├── settings
└── agent
```

Snapshot the Lua-visible table keys, argument behavior, return shapes, warnings,
and sandbox restrictions before extraction. This is a low-semantic-risk
module split once the snapshot test exists.

### 21. Separate database repositories from presentation policy

`core/src/session_db.rs` is 1,832 lines spanning schema migrations,
conversation writes, replay, search, and usage analytics. It also owns UI tab
ordering, labels, and navigation through `ViewMode`
(`core/src/session_db.rs:225-320`). A statistics snapshot performs multiple
sequential aggregate queries (`core/src/session_db.rs:1325-1372`).

Split:

- schema/migrations;
- conversation repository;
- usage repository;
- search/history repository.

Core should accept a typed time window. TUI should own labels, tab order, and
keys. Move usage queries behind the protocol so remote stats use the daemon's
database. Benchmark and, if needed, combine related aggregates in one read
transaction; do not pre-emptively obscure the SQL.

### 22. Remove presentation and terminal knowledge from core

Core's `chat::Message` is used only by TUI presentation, while
`build_chat_history` is actual core behavior (`core/src/chat.rs`). Move the
display types to TUI and leave provider history assembly in core.

Core also optionally depends on `crossterm`
(`core/Cargo.toml:27-35`) and queries terminal width/raw mode in
`core/src/ext/api_ui.rs:233-246` and `core/src/ext/ctx.rs:269-280`. Inject
terminal width and a diagnostic sink/terminal-ownership flag instead. The TUI
already publishes width through the runtime protocol.

Finally, identical core/protocol types should have one owner. `ConfigAction`,
`ConversationLoad`, and `ProcessSnapshot` currently have mirrors and mapping
code. Make wire-identical DTOs protocol-owned; keep core-only execution types in
core. Narrow `pub` visibility behind a supported facade only after checking
external crate usage and retaining deprecation adapters for a compatibility
cycle.

## Dependency, build, and repository debloat

### Safe manifest changes to validate

1. Remove unused TUI `dirs` (`tui/Cargo.toml:27`); there is no `dirs::` use in
   `tui/src`.
2. Remove unused TUI dev dependencies `async-trait` and `serde_yaml`
   (`tui/Cargo.toml:29-31`) after a target-wide compile confirms no hidden
   platform use.
3. Align direct `png` from 0.17 to the 0.18 version already pulled by
   `arboard` (`tui/Cargo.toml:25,41`). This removes `png` 0.17 and
   `bitflags` 1 from the graph if the small encoder API migration passes the
   clipboard tests.
4. Configure `syntect` with `default-features = false` and only
   `["default-syntaxes", "regex-onig"]`. Current code loads default syntaxes
   (`tui/src/ui/render/markdown.rs:17`) and builds its own theme
   (`tui/src/ui/theme.rs:211-287`); it does not use the default theme, HTML,
   plist, or YAML loaders currently enabled by defaults.
5. Remove the empty `[build-dependencies]` table in `core/Cargo.toml:37`.

Measure the dependency graph, release binary, cold build, and syntax rendering
before and after as one PR. Do not remove clipboard image or Wayland features;
those are user features, not bloat.

### Reduce test artifact duplication

Cargo currently links 37 integration-test executables—25 core and 12 TUI.
Because each links substantial static dependencies, the current debug
executables total about 5.15 GB. Consolidate pure integration suites into a few
harnesses with modules. Keep suites that mutate process-global environment or
need isolation separate, or introduce a shared environment lock.

Also consider:

```toml
[profile.test]
debug = "line-tables-only"
```

Measure debugging usefulness and disk reduction before making it the default.
Do not trade independent isolation for flaky environment-variable tests.

### Simplify generated defaults

`core/build.rs:23-105` repeats three nearly identical Lua table generators. The
tools directory is intentionally absent, yet the build script still generates
an empty tool table. Use one parameterized generator and one generated registry,
or a checked-in asset registry with a parity test. Keep the command/lib assets
and installation behavior identical.

### Add an actual pull-request quality gate

The only workflow, `.github/workflows/npm-release.yml`, runs for tags/manual
release (`lines 3-16`). It builds release binaries (`lines 47-52`) but does not
run format, tests, strict linting, headless checks, or Node tests.

Add a PR workflow with:

- format check;
- workspace check for all targets/features;
- workspace tests for all features;
- `bone-core --no-default-features`;
- web UI Node tests;
- strict Clippy after the current nine diagnostics are resolved or explicitly
  justified;
- Linux full coverage, plus Windows/macOS compile or smoke matrices.

Add workspace lint policy so new oversized signatures, unused dependencies,
ignored results, and suspicious I/O do not accumulate silently. Fix or
explicitly justify the lock-file `OpenOptions` warning at
`core/src/session_db.rs:64`; `.truncate(false)` appears to express the intended
lock-file behavior.

### Add focused performance gates

No benchmark, fuzz, or coverage configuration is present. Start small:

- markdown parse/wrap/render at narrow and wide widths;
- bottom-pane layout and status tick;
- 64 KiB process output redraw;
- protocol encode/decode and lag recovery;
- database replay and usage snapshot on large fixtures;
- turn setup with long, image/reasoning-heavy transcripts.

Capture wall time, allocation counts where practical, and output equality.
Coverage percentage is less important than event-trace and state-transition
coverage for this codebase.

## Target architecture

```text
bone binary / bootstrap
├── local services (terminal, clipboard, editor, local shell)
├── embedded daemon adapter OR remote transport
└── TUI
    ├── FrontendClient (request IDs, reliable replies, event sequencing)
    ├── AppModel
    ├── update(AppModel, UiEvent) -> Effects
    ├── key policy -> UiActions
    └── renderer

protocol
├── request/reply envelopes
├── reliable control events
├── coalescible stream/snapshot events
└── typed frontend, job, usage, process, and tool-presentation DTOs

core runtime actor
├── CommandPolicy
├── domain command handlers
├── scoped runtime services
├── Driver
│   ├── RequestRunner
│   ├── StreamAccumulator
│   └── ToolRound
├── TurnJournal
└── repositories
    ├── configuration
    ├── conversations
    └── usage/search
```

This retains every execution mode. The embedded path and socket path use the
same client contract, and local-only capabilities remain explicit.

## Staged implementation plan

### Phase 0: lock behavior down

- Add complete message/replay round-trip fixtures.
- Add two-conversation `bone.submit` isolation tests.
- Record runtime event traces for text-only, tool-only, interleaved text/tools,
  approval, cancellation, live commands, background injection, and reconnect.
- Inject broadcast lag before every completion/control event.
- Add a key-policy matrix across idle, turn, command, and approval phases.
- Add fake-terminal cleanup tests, including error injection and `catch_unwind`.
- Record binary, dependency, build, test-artifact, and render benchmarks.

Exit gate: no production refactor yet; all current behavior is represented by
deterministic tests or an explicitly documented exception.

### Phase 1: correctness and safe wins

- Add `PersistedMessageV2`.
- Scope the Lua submit inbox.
- Add terminal/fullscreen RAII guards.
- Use a unique editor tempfile.
- Deduplicate terminal-width sends and dirty-check process rendering.
- Land the low-risk dependency cleanup and PR CI.

Exit gate: all modes pass; old DB fixtures load; provider payloads remain
equivalent; two managed conversations remain isolated.

### Phase 2: protocol and client reliability

- Introduce request/reply IDs and turn/event sequencing.
- Separate reliable acknowledgements from coalescible deltas.
- Add full-state reconciliation after lag/reconnect.
- Add typed job and usage snapshots plus typed tool/frontend presentation.
- Put one `FrontendClient` in sole ownership of the event receiver.

Exit gate: local and remote trace suites are identical apart from transport;
no nested UI method reads directly from the runtime receiver.

### Phase 3: TUI consolidation

- Introduce `AppModel` state groups behind the existing `App` facade.
- Move all event interpretation into the reducer.
- Replace duplicate key handlers with the phase-aware key reducer.
- Split bottom-pane layout/rendering and retire the direct-prompt branch through
  a compatibility adapter.
- Move presentation-only types out of core.

Exit gate: byte-for-byte rendering snapshots pass at representative widths,
themes, Unicode inputs, prompt states, and tool outputs.

### Phase 4: core turn/runtime consolidation

- Introduce `TurnJournal` and one repository persistence path.
- Extract `TurnState`, `RequestRunner`, `StreamAccumulator`, and `ToolRound`.
- Replace recursive tool/context ownership with catalog/runtime/context types.
- Add one cancellation tree and task ownership model.
- Split command policy/domain handlers from RPC transport.

Exit gate: event order, tool state, persistence, cancellation, and provider
payloads match Phase 0 traces.

### Phase 5: compatibility and data services

- Move to one canonical live `ResolvedConfig`.
- Generate schema/key routing from descriptors.
- Keep legacy files and Lua config surfaces as tested adapters.
- Split Lua capabilities and database repositories.
- Narrow public module visibility after downstream usage is known.

Exit gate: migration fixtures, Lua API snapshots, config snapshots, daemon/TUI
behavior, and release upgrade flows remain compatible.

## Suggested success metrics

- Zero uncorrelated request waiters and zero direct competing reads of the TUI
  event receiver.
- Zero correctness-critical messages on a silently lossy path.
- Exact model-facing message equality across save/reload.
- Deterministic conversation isolation under concurrent managed actors.
- Strict workspace Clippy clean, with narrow documented exceptions.
- Fewer than the current 37 integration-test binaries without test loss.
- Removal of confirmed unused/duplicate dependency paths.
- No release-binary regression from architecture work; measured reduction from
  dependency cleanup.
- No terminal mode/background/alternate-screen leak under injected errors.
- `App`, `Driver`, RPC, and Lua modules become smaller because ownership moved,
  not because methods were spread across files.

## What not to do

- Do not rewrite the TUI around a new framework before event and key traces
  exist.
- Do not remove providers, platform clipboard support, remote mode, migrations,
  or Lua compatibility to claim debloating.
- Do not make broadcast buffers merely larger and call event loss solved.
- Do not merge provider parsers whose wire semantics differ just because their
  SSE loops look similar.
- Do not enable `panic = "abort"` blindly; current panic capture/reporting is
  intentional in `tui/src/main.rs:18-35` and the turn driver.
- Do not delete legacy config readers until supported old fixtures have a
  tested, one-way migration path.
- Do not optimize transcript clones until durable replay and trace equivalence
  are established.

## Recommended first three pull requests

1. **Conversation isolation:** runtime-scoped `bone.submit` queue plus a
   deterministic two-actor regression test.
2. **Durable replay:** `PersistedMessageV2`, schema migration, old-row fallback,
   and exact provider-payload round-trip tests.
3. **TUI safety and baseline:** terminal RAII, unique editor tempfile, PR CI,
   strict-lint cleanup, dependency quick wins, and recorded size/build metrics.

After those, the request-correlated protocol and unified TUI event reducer are
the best foundation for the larger structural work.
