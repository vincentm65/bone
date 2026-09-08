# Native Bone App — Development Plan

Updated: 2026-09-07
Status: Stages 1 and 2 are implemented in the workspace: the connected vertical slice plus the text/performance foundation (Markdown rendering with code blocks and copy, safe links, cached per-row parsing, multiline composer, zoom/font scaling, follow-at-bottom, event-driven idle sleep). Linux compile, unit/integration tests, release build, and a Hyprland window-launch smoke test pass; Stage 2's rendered Markdown was verified by PNG capture (`grim` + crop) and clean SIGTERM shutdown. Interactive Copy-to-clipboard, multiline composer typing, and zoom changes also pass (see Stage 2 validation). Stage 3's multi-conversation alpha milestone passed its fixture-daemon smoke: multi-tab isolation, busy/idle background indicators, per-tab drafts, explicit close (including mid-turn), and Load/Disconnect/Connect reconnect without duplication, with a clean unit shutdown (see Stage 3 validation). Measured renderer performance, cross-platform runtime validation, and an end-to-end tool/approval demo remain pending.

## Goal and scope

Build a fast, polished egui desktop frontend for Linux, macOS, and Windows,
visually based on the existing web UI. Support multiple concurrent conversations
in one app. Keep the TUI and web UI intact. Validate Android/iOS early, then ship
mobile as a remote companion rather than a local coding-agent environment.

Same Cargo workspace, separate executable: proposed package/binary
`bone-desktop` in `native/`. Keep graphics dependencies out of the TUI.

Proposed build behavior (not yet configured):

```sh
cargo build --release                    # existing protocol/core/TUI defaults
cargo build --release -p bone             # TUI
cargo build --release -p bone-desktop     # native app
cargo build --release --workspace        # all Rust workspace packages
```

When adding `native`, explicitly retain `default-members = ["protocol", "core",
"tui"]`. Today all three existing members are the implicit defaults.

## Architectural guardrails

- One authoritative daemon owns sessions, agent loops, tools, approvals,
  cancellation, configuration, extensions, and durable state.
- Native renders `RuntimeEvent`/snapshots and sends `RuntimeCommand`; it does not
  infer mutation success from a click or access core-owned database tables.
- No Node bridge or embedded browser in the native path.
- Separate transport, event reduction, and widgets. Start with internal modules;
  extract a shared client crate only where existing dependencies justify it.
- One attachment/state model per open conversation initially. Hidden tabs retain
  updates but avoid layout/rendering. Correlate tool and interaction IDs.
- Drafts, scroll positions, tab order, and panel sizes are frontend preferences;
  shared session behavior remains daemon-owned.
- Closing a tab is not cancellation. Closing a frontend must not kill a shared
  daemon. Define lifecycle ownership before adding automatic daemon startup.
- Remote connections require a secure tunnel initially or an authenticated,
  encrypted transport before general remote/mobile release.
- Model-generated content is untrusted: control link schemes, local-path actions,
  image loading, and resource limits; do not blindly open supplied URLs/files.
- Concurrent agents can conflict in one working tree; expose workspace identity
  and recommend separate Git worktrees. UI concurrency is not filesystem isolation.

## Stages and exit gates

### 0. Contract, feature, and platform audit

- [x] Inspect transport, callers, recovery, approvals, and dependency boundaries.
- [x] Map actual web workflows to shared APIs and identify web-only state.
- [x] Record platform constraints separately for the frontend and local backend.
- [x] Select provisional graphics stack and check upstream toolchain requirements.
- [x] Define performance scenarios and record available reference hardware.
- [x] Record findings, validation, open decisions, and Stage 1 recommendation.

Exit: a source-backed architecture note, parity checklist, platform matrix, and
measurement plan. Native performance numbers and cross-platform runtime proof
cannot exist until a prototype is available; do not mark them as measured here.

### 1. Connected vertical slice

- [x] Add `native/`, explicit workspace defaults, and package `bone-desktop`.
- [x] Connect to a user-selected, already running daemon; defer automatic startup.
- [x] Load the attached conversation, send a message, and display streaming output.
- [x] Background I/O, typed state reduction, connection/error status.
- [x] Clean frontend shutdown; initial Linux build checks.

Exit: one real conversation completes without the web bridge or duplicated core
logic; I/O does not block the UI; independently running daemon survives app exit.

### 2. Text and performance foundation

- [x] Markdown rendering in `markdown.rs`: headings, ordered/unordered lists
  (tight lists are not wrapped in paragraphs), blockquote indentation, bold/
  italic/strikethrough, inline code, and fenced code blocks.
- [x] Code blocks with a language label and Copy button (`ctx.copy_text`);
  transcript text is selectable.
- [x] Safe links: `is_safe_url` restricts to http/https/mailto; links render
  inline and open via `ctx.open_url(OpenUrl::same_tab)`.
- [x] Multiline composer (`TextEdit::multiline`, 3 rows, hint text), Ctrl+Enter
  or Send to submit, native paste via egui.
- [x] Font scaling/high DPI: zoom slider (0.75-2.0) through
  `ctx.set_zoom_factor`, on top of eframe/egui native high-DPI handling.
- [x] Cached parsing: `parse_cache` keyed by exact row text re-parses only rows
  whose content changed (e.g. the streaming tail) each frame.
- [x] Incremental streaming with coalesced repaints: bounded 256-event drain per
  frame, repaint requested only while events remain; appended rows render under
  follow-at-bottom.
- [x] Follow output only when at the bottom: `stick_to_bottom` with an 8 px
  epsilon against the transcript scroll maximum.
- [x] Idle UI sleeps: no polling repaint timer; repaint is event/input driven
  (a repaint-after is scheduled only while a repair is actively in flight).
- [ ] Variable-height virtualization: the full transcript is laid out each
  frame; deferred until measured long-history results justify it.
- [ ] Cached layout and bounded caches: parse caching only; layout is not cached
  and `parse_cache` entries are not yet evicted. Syntax highlighting is not
  implemented (code blocks render as plain monospace).
- [ ] Lazy large-output rendering/collapse: not implemented.
- [ ] IME and explicit keyboard navigation rely on egui/eframe defaults and were
  not interactively exercised; the smoke test covered plain typing and Enter.
- [x] Interactive Copy-to-clipboard, multiline composer typing, and zoom change
  verified on Hyprland (see Stage 2 validation).
- [x] Exit-gate measurements against the performance scenarios below: renderer
  CPU baseline measured; see "Renderer CPU baseline" and "Runtime measurements"
  in Stage 2 validation below (virtualization still pending before the long
  history P95 budget is met).

Exit: implementation, unit/integration tests, release build, captured
rendered-Markdown screenshots, Copy/zoom/composer interactive proof, and a real
daemon round trip are complete (Stage 2 validation below). Measured renderer
CPU results are recorded; long-history warm frames exceed the 16.7 ms P95
budget, so full performance exit depends on the virtualization/lazy-layout
follow-up captured in the deferred list. Reconsider egui here if selection/
accessibility/input needs require disproportionate custom infrastructure.

### 3. Multi-conversation desktop alpha

- Sidebar/tabs, independent attachments, concurrent models/providers.
- Background working/completed/failed/approval-needed indicators.
- Tool cards, interactive requests, approvals, scoped cancellation and errors.
- New/load, per-conversation drafts/scroll, restart layout restoration.
- Reconnect, authoritative state repair, and explicit tab-close semantics.

Exit: several real conversations run concurrently without state leakage; hidden
approvals are visible; reconnect does not duplicate messages or lose prompts.
This is the first daily-use alpha. Split panes are intentionally later.

Known demo gap (recorded during the Stage 3 alpha smoke): the text-only fixture
mock (`native/test-support/mock_provider.py`) cannot trigger daemon tool calls,
so approvals/questions could not be exercised end-to-end through a real
conversation. The approval surface is implemented and unit-covered (reducer
`needs_approval`, approval cards, Approve/Deny replies, sidebar ⚠ path), but
extending the mock to emit tool/approval events was deliberately out of scope;
an end-to-end approval demo needs a provider that can request a tool call.

Stage 3 status (alpha milestone, 2026-09-07):

- [x] Multi-tab state/reducer additions: tool cards, `last_error`,
      `ignore_first_load`/`reset_new`, `short_title`, `needs_approval`; unit
      tests green.
- [x] Multi-tab desktop rewrite (`native/src/main.rs`): sidebar, per-tab
      connection worker/socket, per-tab reducer/composer/parse cache/draft,
      toolbar Disconnect/Connect, explicit tab-close semantics.
- [x] Build/test validation: `cargo fmt`, `cargo test -p bone-desktop` (13
      passed, 1 ignored), workspace tests, warning-free release build.
- [x] Fixture-daemon window smoke: multi-tab isolation, busy ● / idle ✓
      background indicators, per-tab drafts, real sends persisted by the
      daemon, close-while-busy, Load reopen and Disconnect/Connect reconnect
      without duplication, clean shutdown (details in Stage 3 validation).
- [ ] End-to-end tool/approval demo in the window (blocked on a tool-capable
      provider; see demo gap above).
- [ ] Restart layout restoration and long-session soak.
- [ ] Virtualization/performance work from Stage 2 (unchanged; long-history
      warm frames still exceed the 16.7 ms P95 budget).

### 4. Mobile feasibility gate

May run alongside Stage 3 once transport/state boundaries are stable.

- Minimal secure remote chat on real Android and iOS devices.
- Graphics integration, packaging/signing, keyboard/IME, touch selection/clipboard.
- Safe areas, keyboard insets, suspension and foreground resynchronization.

Exit: demonstrated device builds and explicit go/no-go per platform. If egui is
unsuitable on iOS, preserve protocol/state reuse with a different mobile UI.
No promise of turnkey iOS support before this gate.

### 5. Desktop workflow completion

- Resizable split conversations and file/document/diff canvas.
- Attachments, drag/drop, native file dialogs; distinguish client and daemon paths.
- Model/provider selection, schema-driven settings, extension views/interactions.
- Search/conversation management, keyboard shortcuts and command navigation.
- Local daemon startup/discovery and remote connection management.
- Shared API additions for web-only operations, preserving existing metadata.

Exit: agreed parity checklist passes; remaining omissions are explicit.

### 6. Desktop hardening and release

- Local backend/tool portability testing, separate from client portability.
- Startup/version mismatch/reconnect failures and daemon ownership handling.
- Credential storage, secure transport, accessibility and platform shortcuts.
- Long sessions, large histories, many tabs, failure/recovery testing.
- Installers, signing/notarization, update strategy and upgrade preservation.

Exit: clean-machine install, real workflow, safe shutdown and lossless upgrade on
each advertised platform. Report unsupported local capabilities explicitly.

### 7. Mobile productization

- Adaptive single-column navigation, touch approvals/tool cards/diffs.
- Secure pairing/credential storage, file pickers/share sheets.
- Foreground recovery and optional notifications without assuming an always-live
  background connection; store packaging and real-device validation.

Exit: remote-client releases on platforms that passed Stage 4. A local mobile
backend is a separate project.

- `tui` and `native` use the lightweight `bone-client` crate for the authoritative
  JSONL codec and socket client primitives. `bone-core` retains compatibility
  re-exports and the daemon remains the only runtime authority.
- The Stage 1 `bone-desktop` binary defaults to `127.0.0.1:7878`, connects only to
  an already running daemon, reduces streamed text/tool/approval events, and
  exposes manual reconnect, cancellation, and a multiline composer. A dropped
  socket is surfaced as uncertain prompt delivery; the client never resends it.
- The first native UI intentionally omits tabs, settings, attachments, mobile
  behavior, autostart, Markdown rendering, and conversation-list APIs.

## Stage 0 findings

### Transport and state contract

Source: `core/src/rpc/{mod,codec}.rs`, `core/src/runtime/conn.rs`,
`protocol/src/{event,host,message}.rs`, and actual callers in `tui/src/main.rs`,
`tui/src/ui/app/{mod.rs,stream/mod.rs}`.

- `serve_managed_connection` routes each socket to a conversation actor;
  `LoadConversation`/`NewConversation` reattach that socket. `SessionManager`
  retains independent actors, so concurrent conversations do not need separate
  app or daemon processes. Idle unattached actors have a bounded cache.
- Wire format is JSONL over TCP. `MessageReader`/`write_message` enforce a
  **16 MiB per-message limit**. Large transcript snapshots and base64 images can
  exceed this even if individual messages are small. History pagination/chunked
  snapshots are a potential shared-protocol requirement, not a rendering fix.
- Initial projection order: `FrontendState`, `StateSnapshot`,
  `ConversationLoaded`, then `ViewSnapshot`. Replace transient conversation state
  before installing the full view; never merge a full snapshot as a diff.
- Repair lag with correlated `Synchronize`/`StateSynchronized`, requesting messages
  when needed. Apply session/view/transcript atomically; pending approval/key
  interactions replay after repair. Busy attachment requires synchronization to
  recover a missed stream head. Deduplicate interaction IDs.
- Pair tool activity by `call_id` and turns by `request_id`; do not rely on event
  arrival order. Send approval/cancel on that conversation's attachment.
- `RemoteClient` creates its first event receiver before spawning the pump to
  avoid losing initial replay. Preserve that ordering in any extraction.
- Existing Rust client handles EOF/disconnection but does **not** provide automatic
  reconnect/backoff. Native must implement it, reload the selected conversation
  ID, and ignore initial default-conversation events while restoring that tab.
  Do not automatically resend a prompt whose delivery is uncertain: reattach and
  inspect authoritative state first to avoid duplicate agent/tool execution.
- `FrontendState.host_api_version` describes the host API, not a complete wire
  version handshake. Use explicit compatibility handling; do not treat it as
  proof of every runtime capability.
- `bone serve` defaults to `127.0.0.1:7878`. Current daemon TCP has no auth/TLS;
  `tui/src/main.rs` warns on non-loopback binding. Stage 1 defaults to loopback;
  remote testing uses a secure tunnel, never direct public-port exposure.

### Minimal dependency-boundary recommendation

`bone-protocol` only requires serde, serde_json and num-format. `RemoteClient`
and `SocketConn` currently live in `bone-core`, whose dependencies include
vendored Lua, bundled SQLite and the provider/runtime stack. Linking core solely
for a remote graphical client unnecessarily ties its build to local-agent code.

**Stage 1 recommendation:** make a small, mechanical shared transport extraction
(e.g. `bone-client`) for the existing codec and socket-client primitives, with
core compatibility re-exports where needed. Both core/TUI and native should use
one framing implementation and its tests. Do not copy the codec into native or
move the session manager/daemon into this crate. Keep frontend reconnection and
view reduction in `native` initially. This is a demonstrated dependency boundary,
not permission to redesign RPC.

If this adds `bone-client` as a workspace member, keep the original three default
members; it still builds transitively where used. Before extraction, verify the
exact public API and test placement. Native should depend on `bone-protocol`,
this lightweight transport crate, Tokio and GUI libraries, not on `bone-core`.

### Web parity and API gaps

Source: `webui/bridge.mjs`, `webui/public/{index.html,app.js,ui-core.js}`,
`protocol/src/{event,host,view,message}.rs`. Rows below are an implementation
checklist, not claims that native support exists.

| Workflow | Existing path / ownership | Native stage and action |
|---|---|---|
| Load/new conversation, streaming, reasoning, tools | Runtime commands/events and actor replay | 1, then 3 for complete interaction handling |
| Tabs/background activity | Bridge watch sockets keep other conversations attached | 3: independent native attachments, no bridge dependency |
| Conversation sidebar/list | Bridge `listConversations` queries SQLite directly; no typed runtime list command | **3 prerequisite:** add paginated correlated daemon list API; Stage 1 loads an explicit ID |
| Titles/archive and sidebar search | `webui_conversations` table; bridge SQL list limited to 80 rows; browser search UI | 3 basic list, 5 management/search: shared API, preserve existing title/archive data; define search scope explicitly |
| Approvals, questions, cancel, jobs/processes | Runtime interaction and job/process events/commands | 3: render daemon previews and scope replies correctly |
| Settings/provider/model/mode | Revisioned runtime config commands/snapshots | 5: reuse schema and revision conflict handling |
| Usage statistics/catalog/setup | Correlated daemon `HostRequest`/`HostResponse` | 5: native presentation, no frontend DB/catalog logic |
| Theme, panes/task checklist | `ViewSnapshot`/`ViewDiff` and frontend theme state | 1 theme, 5 complete declarative-view rendering |
| Images/text attachments | Browser file conversion; runtime prompt images plus submitted text | 5: native pickers/conversion, byte limits including JSON/base64 overhead |
| File/diff canvas from tool output | Browser canvas rendering from runtime tool data | 5: native cached viewer; retain source IDs and safe links |
| Load full workspace file | Bridge `/api/file` reads bridge-host filesystem under its launch workspace | 5: bounded daemon-host file API with workspace authorization; remote path is not a client-local path |
| Editor/download actions | Browser/platform behavior, not a portable daemon operation | 5: explicit local/remote semantics; never silently open a remote path locally |
| Drafts/panel sizes/preferences | Browser localStorage | 3/5: client-only persistence, not shared configuration |
| Daemon restart | Bridge can restart only a daemon it manages | 5/6: explicit ownership, no killing a daemon another client owns |

Do not directly reuse the bridge's SQL/path handlers in a native client. Add only
the required shared APIs, update web consumers where ownership moves, and test
preservation of existing metadata. In particular, the sidebar API is needed in
Stage 3, earlier than the broader Stage 5 parity work.

### Platform support matrix and concrete constraints

| Platform | Native client | Existing local backend evidence / remaining work |
|---|---|---|
| Linux Wayland/X11 | Upstream eframe support; not built here yet | Current development host; runtime/tool behavior still needs native workflow tests |
| macOS Intel/ARM | Upstream eframe support; no local runtime proof | Existing release workflow targets both; real machine input, permissions, subprocess and packaging validation required |
| Windows x86_64 | Upstream eframe support; no native runtime proof | Existing release workflow targets MSVC; PowerShell/process-tree branches already exist; real workflow validation required |
| Android | Upstream eframe support; device integration untested | Remote-client only initially; do not pull local runtime into mobile dependencies |
| iOS | Not advertised by eframe; integration unproven | Stage 4 device spike; remote-client only; fallback UI remains possible |

Verified source evidence, not inferred support claims:
- `.github/workflows/npm-release.yml` already builds release targets for Linux
  x86_64/ARM64, macOS Intel/ARM64 and Windows x86_64. This is a configured build
  matrix, not evidence that current CI passed or GUI/tool workflows were tested.
- `core/src/tools/shell.rs::detect_shell_command` selects pwsh/PowerShell on
  Windows and `bash -lc` elsewhere. Process termination has Unix process-group
  and Windows `taskkill` branches. Do not assume all platforms require bash or
  that Windows support must be written from scratch. Installed shell/tool and
  script semantics still need platform tests.
- `tui/src/main.rs` launches the web bridge with Node; the native frontend avoids
  that requirement, but a separately managed local daemon still needs packaging.
- Optional installed extensions/tools are outside this source-only audit. Do not
  promise their desktop automation, shell scripts or external binaries work on
  every OS simply because the native window does.

**Stage 0 decision:** proceed to a small Stage 1 desktop prototype after review.
No backend redesign is needed. Shared transport extraction is justified; sidebar
listing and authorized remote file access need scoped APIs at their owning stages.
Cross-platform linking/runtime proof and actual performance measurements remain
explicit prototype/release gates.

## Provisional dependency decision

Use matching `eframe`/`egui` 0.36.1 for the Stage 1 trial, with eframe's wgpu
backend. Upstream tagged workspace requires Rust 1.95 / edition 2024, matching
this machine's rustc 1.95.0. Pin the resolved graph in Cargo.lock during Stage 1;
no dependencies or manifests are changed in Stage 0.

- Let eframe select compatible wgpu/winit versions rather than independently
  adding another graphics stack (tagged upstream uses wgpu 30.0 / winit 0.30.13).
- Retain Linux Wayland and X11 integration and accessibility support. Verify the
  released feature graph and system library requirements during compilation.
- Tokio should match the existing workspace resolution where compatible.
- Defer Markdown/highlighting library selection until Stage 2 quality tests.
- Android is advertised by eframe; iOS is not in its advertised platform list.
  Treat iOS integration as unproven, not impossible.

Sources checked 2026-09-07:
- https://docs.rs/eframe/0.36.1/eframe/
- https://github.com/emilk/egui/blob/0.36.1/Cargo.toml
- https://github.com/emilk/egui/tree/main/crates/eframe

## Performance acceptance plan

These are proposed budgets, not benchmark results. No native renderer exists yet.
Use synthetic/redacted fixtures, not private conversations or live paid inference.

| Scenario | Fixture and check |
|---|---|
| Startup | Empty app and restoring ten tabs; measure process start to first usable frame separately from daemon attach/history load |
| Long history | 10,000 mixed messages / roughly 8 MiB serialized snapshot for current transport; separate 20 MiB stress fixture for renderer-only testing until snapshot chunking exists; scroll, resize, select/copy and switch tabs |
| Streaming | Four active conversations, 100 small deltas/second each; scroll one while another requests approval |
| Large output | 100,000-line tool result and a large diff; collapsed load, expand, scroll, copy; no full-frame unbounded work |
| Reconnect | Drop connection during streaming and during approval; restore authoritative state and scoped interactions |
| Idle/soak | Ten open tabs idle, then repeated open/close/load cycles for 30 minutes; check repaint activity, CPU and retained memory |

Initial targets for review:
- P95 UI frame work <=16.7 ms at 60 Hz during visible scrolling/streaming.
- Warm launch to first usable shell <=1 second; record cold startup separately.
- Idle process CPU <1% of one logical core over 60 seconds, no polling repaint loop.
- Initial memory budget: <=250 MiB idle RSS and <=500 MiB for the long-history
  fixture; report GPU allocations separately and revise only with measured cause.
- No sustained memory growth across repeated equivalent load/close cycles after
  warmup; document intentional caches and their bounds.

Measure release builds at fixed window size/scaling and record OS, GPU adapter,
driver, refresh rate, fixture bytes, run count, and P50/P95 times. Record at least
five startup runs. These synthetic budgets do not predict provider/tool latency.

Available Linux development host (not a low-end product baseline): AMD Ryzen 9
9900X, approximately 30 GiB RAM, Linux 7.1.3 x86_64. Enumerated GPUs: two NVIDIA
RTX 3090 devices and AMD integrated graphics; the actual rendering adapter must
be recorded when benchmarking. macOS/Windows and a modest laptop still need
reference machines. Installed Rust targets include Linux x86_64, Windows MSVC
x86_64 and wasm32; target installation does not prove linking or runtime support.

## Validation and release guardrails

- Reducer/protocol tests for snapshots, recovery and conversation isolation.
- Real native-window smoke tests for typing, streaming, resize, approval/cancel,
  reconnect and shutdown on each desktop OS; real devices for mobile.
- Preserve existing TUI/web behavior. Shared changes require focused existing
  tests; user-facing TUI changes also require isolated-BONE_DIR tmux smoke tests.
- Do not bundle catalog items, redesign extensions, add a full IDE/terminal, or
  replace the existing frontends as part of this effort.
- Detached windows are deferred beyond the initial desktop release.

Stage 0 validation:
- `cargo metadata --no-deps --format-version 1`: confirmed current packages and
  default workspace members; no native package exists yet.
- `cargo test -p bone-protocol --locked`: **21 passed**.
- `cargo test -p bone-core rpc::codec --locked`: **3 passed** (other tests filtered).
- No native build, GUI smoke test, cross-platform runtime test or performance
  benchmark performed: this change is a plan/source audit only.
- External lookup limitation: crates.io API returned `curl: (22) The requested URL
  returned error: 403`; the empty response then caused Python `JSONDecodeError`.
  Version/toolchain information was verified through docs.rs and tagged upstream
  Cargo.toml instead.
- One delegated audit failed with provider `HTTP 400 Bad Request`:
  `request (120828 tokens) exceeds the available context size (120064 tokens)`.
  Its web/platform portion was completed with targeted direct source inspection.
- Initial path probes reported missing `webui/src`, `core/src/foreground*`,
  `core/src/tools.rs`, `core/src/tools/terminal*`, and `core/src/tools/computer*`.
  These paths were not used as evidence; actual source locations are cited above.
- Only this plan is intentionally added. No application/configuration changes,
  dependency downloads into the workspace, commits, or pushes.

## Stage 1 validation (2026-09-07)

Files added/changed:
- New crates: `client/` (`bone-client`) and `native/` (`bone-desktop`).
- Workspace `Cargo.toml`: `members` now includes `client` and `native`;
  `default-members` retained as `["protocol", "core", "tui"]` so default builds
  skip graphics dependencies.
- Shared transport extraction: `core/src/rpc/codec.rs` is a re-export of
  `bone_client::{MAX_LINE_BYTES, MessageReader, ReadError, write_message}`;
  `core/src/rpc/mod.rs` re-exports `RemoteClient`; `core/src/runtime/conn.rs`
  keeps the `RuntimeConn` trait with `impl RuntimeConn for SocketConn`;
  `tui/src/main.rs` and `core/src/rpc/rpc_tests.rs` dropped now-unused imports.

Results (this host, Linux x86_64, rustc 1.95.0):
- `cargo test -p bone-client`: **1 passed**.
- `cargo test -p bone-core --lib -- rpc::`: **52 passed**; `-- codec`: **5 passed**.
- `cargo test --workspace`: **all green** (no failures; includes `bone-desktop`
  5 unit/integration tests: 4 `state` reducer cases + 1 tokio loopback connection
  test covering connect, streamed event, command send, and clean disconnect/EOF).
- `cargo build -p bone-desktop --release`: **success, no warnings**.
- `cargo build -p bone` (TUI): **clean**. `cargo fmt --check` on native sources: clean.
- GUI smoke test on Hyprland (Wayland `wayland-1`, two physical monitors DP-4/DP-5):
  - `computer` status/inspect-all-monitors run before launch (no existing
    `bone-desktop` window; DP-4 Firefox, DP-5 foot terminal).
  - Launched `target/release/bone-desktop`; a window with class/title `Bone Desktop`
    appeared on DP-5 and stayed stable for ~90 s with an empty stderr log
    (eframe/wgpu initialized; a graphics failure would have exited with no window).
  - `SIGTERM` produced a clean exit within 1 s; no lingering `bone-desktop` process.
  - Limitation: this runtime is built without the PNG codec, so `computer observe`
    screenshots are unavailable; the UI was verified by window presence/stability and
    clean shutdown, not by pixel capture.

Known omissions in this slice (intentional, carried to later stages):
- No automatic reconnect/backoff; a dropped socket is surfaced as uncertain prompt
  delivery and the client never resends it (manual reconnect only).
- No Markdown/code rendering, no conversation list/sidebar, no tabs, settings,
  attachments, image display, or autostart. Single attached conversation by explicit
  ID (or the daemon default). Ctrl+Enter / Send to submit; minimal transcript rows.
- No real-daemon end-to-end chat round-trip executed here (needs a running daemon,
  conversation, and provider); transport and reducer are covered by the tests above.
- Cross-platform (macOS/Windows) runtime and performance benchmarks are pending.

## Stage 2 validation (2026-09-07)

Files added/changed (all in `native/`):
- `native/src/markdown.rs` (new, ~523 lines): pure `parse_markdown(&str) ->
  Vec<Block>` and `render_blocks(...)`; `is_safe_url` gate; 5 unit tests.
- `native/src/main.rs`: `mod markdown`, `stick_to_bottom`, per-row
  `parse_cache`, `BONE_DESKTOP_DEMO` seeding, zoom slider, cache-driven
  transcript rendering inside a `stick_to_bottom` scroll area.
- `native/Cargo.toml`: added `pulldown-cmark = "0.13"`; `Cargo.lock` updated.

Results (this host, Linux x86_64, rustc 1.95.0, Hyprland on Wayland):
- `cargo test -p bone-desktop`: **10 passed** (5 `markdown`, 4 `state`, 1
  `connection` loopback covering connect, streamed event, command send,
  disconnect/EOF).
- `cargo test -p bone-client`: **1 passed**.
- `cargo test --workspace`: **all green** - 46 suites, no failures or errors.
- `cargo build -p bone-desktop --release`: success, no warnings.
- `cargo fmt -p bone-desktop -- --check`: clean.

Rendered-Markdown screenshot verification (this session):
- Display initially appeared offline (empty `computer` monitor list); recovered:
  DP-4 and DP-5 both report `dpmsStatus: 1`. `hyprctl` requires
  `HYPRLAND_INSTANCE_SIGNATURE`, which was recovered from a live client's
  environ (`/proc/4159/environ` of `hypridle`) and also located the instance.
- `grim` uses single-dash getopt and rejects `--help`/`--version` ("invalid
  option -- '-'"); `grim -h` and plain/output captures work.
- Launched the `BONE_DESKTOP_DEMO=1` release build via `setsid` (survives
  shell-tool process-group cleanup) with `WAYLAND_DISPLAY=wayland-1
  XDG_RUNTIME_DIR=/run/user/1000`; window `Bone Desktop` appeared on DP-5 at
  logical (1284,4), 1264x1412, monitor scale 1.5.
- `grim -o DP-5` plus an `ffmpeg` crop of the window region, viewed as a PNG,
  confirmed every Stage 2 Markdown feature renders: H1/H2 headings, bold,
  italic, strikethrough, inline code, unordered and ordered lists, a blue
  hyperlink, an indented blockquote, and a `rust`-labelled code block with a
  Copy button and monospace body; also the zoom row, connection fields,
  disabled Send/Cancel, and the multiline-composer hint.
- `SIGTERM` produced a clean exit within ~2.5 s; no lingering process and an
  empty stderr log.

Interactive verification (follow-up, same host):
- Woke both monitors; ran the release demo as transient user unit `bdt-demo`
  on DP-4 at logical (3844,4), 1264x1412, monitor scale 1.5.
- `computer observe` still fails with `model screenshot resize requires
  ctx.codec.png_resize`; used `grim` captures, viewed via `read_file`, and
  `ydotool`/`wtype` instead. No application source changes were needed.
- Corrected the input diagnosis: `ydotool mousemove` is relative by default;
  positive moves at the bottom-right corner clamp. Negative moves require `--`.
  The user's flat sensitivity of 2 gives two logical pixels per relative unit;
  configuration was left unchanged. `click 0x0` does nothing; `click 0xC0`
  sends the full left-button down/up sequence. The existing `ydotool` user
  service was restarted during diagnosis and remains running.
- Copy: clicked the code-block Copy button; `wl-paste --no-newline` matched the
  demo code byte-for-byte (48 bytes, including newlines).
- Composer: clicked the text box, typed `Stage 2 composer check`, pressed Enter,
  and typed `Second line stays in the draft.` Both lines were visibly present;
  Enter did not submit. Send/Cancel remained disabled while disconnected.
- Zoom: clicked the slider; the displayed factor changed from 1.00 to 1.55,
  text and controls enlarged, and the two-line draft was preserved.
- Evidence: `/tmp/tidy-composer-proof-small.png` and
  `/tmp/tidy-zoom-proof-small.png` (temporary screenshots, not repository files).
- Stopped `bdt-demo` through systemd (SIGTERM); journal confirmed shutdown with
  no application errors, and `pgrep` confirmed no remaining desktop process.

Real-daemon end-to-end round trip (this session, isolated fixture):
- Added untracked test support under `native/test-support/`:
  `mock_provider.py` (OpenAI-compatible `text/event-stream` server at
  127.0.0.1:17879; streams "Desktop ", "local fixture ", "response."; prompts
  containing "slow" sleep 2 s/chunk for cancel/concurrency cases) and
  `daemon_smoke.py` (raw JSONL socket client for the daemon port).
- Ran a release `bone serve` as a transient user unit with an isolated
  `BONE_DIR=target/desktop-validation` whose `providers.yaml` defines provider
  `desktop-fixture` pointing at the mock provider. Transport smoke passed:
  stream + finish in 0.309 s; reconnect/load preserved history; two concurrent
  actors on separate `conversation_id`s with cancellation of one scoped so the
  other finished independently; the daemon survived client shutdown.
- Launched the release native window as a transient user unit on DP-4 with the
  same isolated `BONE_DIR`. Screenshots (`connect.png`, `chat.png` and
  `-small` variants under `target/desktop-validation/`) show: typed daemon
  address, Connected state, submitted prompt "Native window live daemon smoke",
  and the streamed assistant reply rendered in the transcript.

Runtime measurements (this host; release build; active daemon conversation):
- 60 s sample of the running window with no further user input (the app window
  retained focus and held one idle daemon conversation that had just completed a
  streaming turn): **CPU 4.617% of one logical core, RSS 158.93 MiB**. The <1%
  idle budget still needs a pure-idle re-check and the measured value is
  recorded here as provisional, not as a gate result.

Renderer CPU baseline (`cargo test --release -p bone-desktop
renderer_baseline -- --ignored --nocapture`; CPU/layout only, no GPU, one
window 1264x1412, full-layout clone behavior; recorded via
`native/src/perf_tests.rs`):
- long-history-8MiB (8.08 MiB, 10,000 messages): parse 10.77 ms,
  cold layout 72.0 ms, warm p50 36.68 / p95 37.16 ms.
- renderer-stress-20MiB (21.04 MiB, 10,000 messages): parse 8.29 ms,
  cold layout 57.9 ms, warm p50 27.83 / p95 27.99 ms.
- tool-100000-lines (1.70 MiB, single block): parse 0.79 ms, cold layout
  10.5 ms, warm p50 0.24 / p95 0.27 ms.
- Conclusion: long-history warm frames 28-37 ms exceed the 16.7 ms P95 budget;
  the transcript must move to variable-height virtualization and/or bounded
  caches before Stage 2 performance exit can be claimed.

Deferred beyond this slice (intentional, matching scope):
- Variable-height virtualization, cached layout, bounded/evicting caches,
  syntax highlighting, lazy large-output/collapse, and image display. The
  measured long-history warm-frame cost above makes virtualization the first
  performance follow-up before claiming Stage 2 performance exit.
- A pure-idle (no active turn, editor unfocused) 60 s CPU/RSS re-check, plus
  startup-to-first-frame and reconnect/soak measurements against the
  acceptance scenarios.
- macOS/Windows runtime validation.

## Stage 3 validation (2026-09-07)

Harness (same host as Stage 2; Linux x86_64, Hyprland, window on DP-4 at
logical (3844,4), 1264x1412, scale 1.5): fixture mock provider
(`native/test-support/mock_provider.py`, 127.0.0.1:17879, streams 3 chunks,
~2 s each when the prompt contains "slow"), release `bone serve` with isolated
`BONE_DIR=target/desktop-validation` on 127.0.0.1:17878, and the release
`bone-desktop --connect` window — all as transient user units
(`bone-desktop-fixture`, `bone-desktop-test-daemon`, `bone-desktop-stage3`).
The daemon SQLite store (`target/desktop-validation/data/conversations.db`)
is the ground truth for what was actually sent/persisted. Conversation rows
were driven with `wtype`/`ydotool` and verified by OCR/pixel analysis of
`grim` captures.

Files added/changed:
- `native/src/state.rs`: multi-conversation reducer additions (tool cards,
  `last_error`, `ignore_first_load`/`reset_new`, `short_title`,
  `needs_approval`) with 7 unit tests.
- `native/src/main.rs`: multi-tab desktop (sidebar with per-tab rows/glyphs/
  close ✕, per-tab connection worker + reducer + composer + parse cache +
  draft, toolbar address/Connect/Disconnect, Load ID/Open, explicit
  close-while-busy semantics), 13 tests pass.
- `native/test-support/mock_provider.py`: fixture SSE mock used above.
- `native/Cargo.toml`: already staged from prior slices; no further changes.

Results (verified this slice):
- Real send path through the window: typed prompts land as user+assistant rows
  in the daemon DB; the sidebar row title becomes the conversation's first
  user message; the composer clears and the transcript renders the exchange.
- Busy/idle background indicators: while a tab's turn is streaming its whole
  sidebar row (glyph + title) turns green ● (green-dominant pixels measured,
  criterion g-r>45 && g-b>35); after the turn finishes it returns to gray ✓
  (green pixel count 0). "Turn: thinking/Ready" status line tracks the
  selected tab.
- Per-tab composer drafts: text typed in one tab is retained when switching to
  another and back; no DB pollution from unsent drafts.
- Transcript isolation: a slow exchange on conv5 and a fast exchange on conv6
  stream concurrently; each tab's transcript shows only its own conversation
  (DB rows match per `conversation_id`), no leakage in either direction.
- Close while busy: conv6's ✕ was clicked ~2 s into a ~6 s slow stream; the
  tab disappeared from the sidebar, the app stayed alive, and the daemon
  finished and persisted the full exchange (conv6 gained the user + assistant
  rows) after the tab closed.
- Load reopen: with all tabs closed, Load ID 5 + Open + Connect re-attached
  conv5 with its history rendered exactly once (each unique message marker
  counted once in the transcript; DB conversation row count unchanged at 8).
- Disconnect/Connect cycle: after Disconnect the socket read "Disconnected";
  after Connect the transcript was intact with no duplicate messages and
  "Socket: Connected / Turn: Ready".
- Clean shutdown: `systemctl --user stop bone-desktop-stage3.service` exited
  without panic/crash in the journal and left no process; the fixture and
  test-daemon transient units were then stopped.
- Evidence (under `target/desktop-validation/`): `stage3-14-busy.png`
  (busy row green while another tab selected), `stage3-15-idle.png` (gray ✓
  after), `stage3-19.png`/`stage3-20.png` (per-tab transcript isolation),
  `stage3-22.png`, `stage3-23-busy.png` (row green on the tab closed mid-turn),
  `stage3-25-close-busy-result.png` (sidebar after the busy close),
  `stage3-26-reconnect.png` (reopened/reconnected transcript). The older
  `stage3-bg-busy.png` predates the input-coordinate fix and is unreliable.

Not verified in this slice (kept explicit):
- End-to-end tool calls/approvals in the window (mock is text-only; see the
  demo-gap note in the Stage 3 scope above). The UI Cancel path was not
  clicked; daemon-side scoped cancellation was covered by the Stage 2
  transport smoke, and close-while-busy sends Cancel by design.
- Restart layout restoration and long-session/reconnect soak.
- Renderer performance: unchanged from Stage 2 (warm long-history frames
  28-37 ms still exceed the 16.7 ms P95 budget; virtualization not claimed).

## Review decisions before Stage 1

1. Approve the separate `bone-desktop` executable and preserved Cargo defaults.
2. Approve explicit connection to an existing local daemon for the first slice.
3. Accept proposed performance budgets as trial gates, subject to measurements.
4. Arrange macOS/Windows test machines and an iPhone for the early mobile spike.
5. Review Stage 2 text/performance results before committing to full parity.
