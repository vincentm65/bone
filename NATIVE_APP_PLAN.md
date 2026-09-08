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

## Desktop UX Layer 1 — auto daemon lifecycle + auto-connect (2026-09-08)

Out-of-band UX slice, requested ahead of the remaining Stage-5 work: the desktop
app should need no address typing or Connect click for the local case.

### Change summary (native/ only)
- New `native/src/daemon.rs`: `DEFAULT_ADDRESS=127.0.0.1:7878`, retry budget
  (`MAX_DAEMON_RETRIES=10`, 600 ms), address normalization (`ensure_port`), and
  `spawn_daemon` (`serve --listen <addr>`, appended log, detached process group,
  stdin nulled). Pure helpers are unit-tested.
- `native/src/main.rs`: every tab starts `auto`; `begin()` connects all tabs on
  open. On a refused loopback connect the coordinator spawns the sibling
  `bone` binary (or `$BONE_DESKTOP_DAEMON`), shows an amber "… Starting daemon"
  pill, retries, then flips to green "● Local daemon" once a tab connects.
  Failures land in a red "Daemon offline" pill plus a `Daemon:` notice. The
  address TextEdit + Connect/Disconnect moved out of the toolbar into a
  "Server…" dialog. Restart layout restore is unchanged and still debounce-saved.
- Lifecycle decision (guardrail "one authoritative daemon"): the app is a client,
  not a supervisor — a spawned daemon deliberately **outlives** the app and
  shares the app environment (BONE_DIR inherited), so it serves the same
  conversations after the frontend closes. Daemon log:
  `<state-dir>/logs/daemon.log` (state dir = parent of the layout file).

### Status
- Unit tests: `cargo test -p bone-desktop` = 39 passed, 0 failed, 1 ignored
  (re-run on a clean tree 2026-09-08).
- Release binaries rebuilt 2026-09-08 (now including the Layer-2 core/protocol)
  for the consolidated smoke.
- `cargo fmt -p bone-desktop` clean.
- **Window smoke: NOT yet run by automation** — the display GPU (GPU1) has no
  spare VRAM while the local llama-server holds ~20.6 GB, so a new wgpu device
  fails with `Wgpu … Device(OutOfMemory)` (no lavapipe/Xvfb fallback installed).
  Handed to the user to run later (runbook below).

### Window smoke runbook (user-run; ~3 minutes)
1. Ensure VRAM: display GPU needs headroom for one wgpu device (llama-server's
   context currently blocks it). No other GPU window may be starting.
2. State is pre-staged at `target/l1-smoke/` (survives until deleted):
   - `layout.txt` — header v1, `address 15 127.0.0.1:17900`, `selected 0`,
     `tab new` (fresh tab; free port, no daemon on it).
   - `bone-home/providers.yaml` — `active: desktop-fixture` → mock provider
     `http://127.0.0.1:17879` (mock_provider.py still running).
   - `ocr.py snapshot <tag>` → grim DP-4 + negate + tesseract into
     `target/l1-smoke/evidence/`.
3. Launch from the repo root in a Wayland session:
   ```
   env BONE_DIR=$PWD/target/l1-smoke/bone-home \
       BONE_DESKTOP_STATE=$PWD/target/l1-smoke/layout.txt \
       ./target/release/bone-desktop
   ```
4. Expect (no typing, no Connect click):
   - Pill goes red/amber briefly, then **green "● Local daemon"**.
   - A sibling daemon appears: `ss -ltn | grep 17900` →
     `bone serve --listen 127.0.0.1:17900` (spawned from `target/release/bone`).
   - `target/l1-smoke/logs/daemon.log` exists (state-dir logs).
   - After ~1 s the fresh tab auto-connects and pins a conversation; `layout.txt`
     now says `tab load 1` (debounce-saved; wait ~1 s before quitting).
   - OCR snapshot for evidence (pill text, tab header).
  4b. Picker (Layer 2, consolidated into this run):
     - While the pill is still starting, the sidebar's **Recent** section shows
       "Waiting for the daemon…".
     - Once the tab connects it shows "No conversations yet." (the staged
       BONE_DIR is empty — the 2026-09-07 wipe removed the old fixture
       conversations).
     - Type a short message in the composer and send it (Ctrl+Enter). The mock
       provider at 127.0.0.1:17879 must still be running for a reply.
     - Click the **↻** button next to "Recent": the new conversation now appears,
       titled with your prompt, with a weak "N messages · <timestamp>" hint.
     - Click the entry: the current tab (holding that conversation) is selected —
       no new tab is created, and the entry gains a "✓".
     - Click "+ New conversation" (a second, empty tab opens), then **↻** again:
       the list keeps the first entry most-recent-first; the empty tab's
       conversation shows as "(new)" once it has an id.
     - OCR snapshot for evidence (Recent list, titles, hints).
5. Quit the app (close window or SIGTERM). Verify the spawned daemon **keeps
   listening** on 17900 (outlives the app by design).
6. Kill that daemon, relaunch the app with the same env — the layout now restores
   `tab load 1` at `127.0.0.1:17900`, the refused connect re-spawns the daemon,
   and the pill goes green again (restart-restore + spawn-on-refused with the
   real binary).
7. Cleanup: quit the app, kill the spawned 17900 daemon. Optionally
   `rm -rf target/l1-smoke`. Rebuild state dirs are disposable.

Note: the previous fixture store under `target/desktop-validation` (providers
copy, conversations incl. conv 7, scratch OCR tooling, evidence) was removed by
an intentional `git clean`-style wipe on 2026-09-07 ~21:37 while the fixture
daemons were running; only tracked source + `.bone-rust` survive. Recreate a
BONE_DIR from `.bone-rust/providers.yaml` (redacting keys) when fixture
conversations are needed again.

## Desktop UX Layer 2 — recent-conversation picker (2026-09-08)

Goal: kill the numeric "Load <id>" field in the desktop app. Users pick a recent
conversation from a sidebar by title; the numeric id stays internal (layout
`tab load <id>` is unchanged).

### Design decision: host-scoped request, NOT a per-conversation command
`ListConversations` was planned as a `RuntimeCommand` in `protocol/event.rs`. On
grounding in the code, the cleaner home is the **host control plane**:
- The `HostRequest`/`HostResponse` pair is already the correlated, daemon-global
  request/response channel, and its documented authority is "its usage database"
  — the *same* `conversations.db` we list. `HostRequest::Stats` already opens it.
- The daemon loop already routes `HostRequest` → `HostService::execute` →
  publishes `RuntimeEvent::HostResponse { request_id, response }` (rpc/mod.rs
  ~2067/2101). So a host request needs **no** serve-path, daemon-loop, codec, or
  tui changes.
- The native connection layer is generic (`Command::Send(RuntimeCommand)` /
  `Event::Runtime(RuntimeEvent)`), so sending a host request and receiving the
  response needs no connection-layer change.

Net: this touches only `protocol/` (signed off) + `core/src/host.rs` +
`core/src/session_db.rs` (signed off for Layer 2) + `native/src/main.rs`.

### Wire shape (protocol/src/host.rs)
- `ConversationMeta { id: i64, title: String, updated_at: String,
  message_count: i64, provider: String, model: String }`.
- `HostRequest::Conversations { limit: u32 }` — `limit=0` selects the daemon
  default.
- `HostResponse::Conversations(Vec<ConversationMeta>)`.

### Metadata source (core/src/session_db.rs)
The `conversations` table has only `id, started_at, ended_at, provider, model`
(no title, no updated_at). To avoid a schema migration:
- `title` = first non-empty `role='user'` message `content` (ordered by `seq`),
  truncated/one-lined in Rust; fall back to "(new)" when a conversation has no
  user text.
- `updated_at` = `MAX(messages.created_at)` for the conversation, else its
  `started_at` (both are `utc_now()` ISO strings, so they string-sort).
- `message_count` = `COUNT(messages)`.
- Ordering: most-recent-first (`updated_at DESC, id DESC`), `LIMIT ?`.
New method `SessionDb::recent_conversations(limit) -> rusqlite::Result<Vec<ConversationMeta>>`;
the query lives here (protocol stays free of core types, mirroring how
`stats()` re-exports protocol types).

### Native picker (native/src/main.rs)
- The app owns one in-flight request: `conversations_request: Option<(tab_id,
  request_id)>`, `conversations: Vec<ConversationMeta>`, `conversations_loaded`.
- `poll_conversations()` runs at the end of every `drain_all`: once any non-demo
  tab is connected it sends
  `RuntimeCommand::HostRequest { request_id, request: HostRequest::Conversations { limit: 0 } }`
  through that tab (request id from the tab's `state.next_id()`). No busy loop:
  the connect event's repaint re-runs the poll. If the sending tab loses its
  socket before the response, the request is abandoned and re-issued on the next
  connected tab. Demo mode never issues the request.
- `Tab::handle_event` captures each `RuntimeEvent::HostResponse` in
  `tab.host_response` (the tab reducer ignores it via its catch-all);
  `drain_all` then applies the response only when (tab id, request id) matches
  the pending one: `Conversations(vec)` fills the list, `Error` sets a sidebar
  notice, other host responses are dropped.
- Sidebar: the numeric "Load <id>" field is gone. Under "Recent" (with a "↻"
  refresh button) the app shows "Waiting for the daemon…", "Loading…", "No
  conversations yet.", or the recent-first list — title (with a "✓" when a tab
  already holds it) plus a weak `message_count · updated_at` hint. Clicking an
  entry calls `open_conversation(id)`: selects the existing attached tab if
  there is one, else opens a fresh `Intent::Load(id)` tab and connects it. The
  numeric id stays internal (layout `tab load <id>` is unchanged).

### Status
- **Implemented** (2026-09-08). Host dispatch, wire shape, metadata query, and
  native picker as described above.
- Tests (re-run on this tree 2026-09-08):
  - `protocol/src/host_tests.rs` — `Conversations` request/response round-trips,
    `{"conversations":{}}` → limit 0 default.
  - `core/src/session_db_tests.rs` — `recent_conversations_orders_by_last_activity_and_derives_titles`
    (updated_at ordering beats id order, first-user titles, 60-char ellipsis,
    "(new)" fallback, message counts, provider/model, limit) + empty-DB case.
  - `core/src/host_tests.rs` — service-level list (default + limit) and
    `HostResponse::Error { code: Unavailable }` when the DB cannot open.
  - `native` — 4 new tests: request issued once on the first connected tab,
    correlated response populates the list and `open_conversation` reuses tabs,
    request abandoned when the sending tab disconnects, no-op in demo mode.
  - Totals: `cargo test -p bone-protocol -p bone-core -p bone-desktop` all green;
    `cargo test -p bone-desktop` = 43 passed, 0 failed, 1 ignored.
- `cargo fmt` clean for every touched file (the five pre-existing fmt drifts in
  `core/src/config/*`, `core/src/runtime/driver.rs`, `core/tests/session_sink_test.rs`,
  `protocol/src/config.rs` were left untouched on purpose).
- Release binaries rebuilt with the new core/protocol
  (`cargo build --release -p bone-desktop -p bone`) for the consolidated smoke.
- **Consolidated window smoke: run by the user (2026-09-08) — passed.** The
  Layer-1 + Layer-2 runbook completed on a display machine, including the
  picker verification step (4b).

## Stage 5, slice 1 — model/provider selection (2026-09-08)

First Stage-5 vertical: let the desktop pick the daemon's active provider and
edit the active provider's model. No protocol or core changes — the existing
runtime surface is sufficient (`GetConfig` → broadcast `ConfigSnapshot`;
`SetActiveProvider` / `UpsertProvider` with revision checks → broadcast
`ConfigChanged` / `ConfigMutationRejected`), so this slice is `native/` only.

### Data flow (native/src/main.rs)
- `Tab` buffers daemon config traffic the same way it buffers host responses:
  `config_snapshots: Vec<(ConfigSnapshot, bool /*restart_required*/)>` and
  `config_rejections: Vec<String>`, filled in `handle_event` before the
  (ignoring) reducer, drained app-level in `drain_all`.
- `DesktopApp` holds `config: Option<ConfigSnapshot>` (latest authoritative
  snapshot — any connected tab can refresh it, since `GetConfig` responses are
  broadcast, not request-correlated) and `config_request: Option<tab_id>` for
  the in-flight fetch. `poll_config()` (run from `drain_all`, no timers)
  issues `GetConfig` once any tab is connected and abandons the pending fetch
  if its tab loses the socket.
- A `ConfigMutationRejected` clears `config` (forcing a refetch of the real
  revision) and shows the daemon's error in the dialog.

### UI
- Toolbar pill `⚙ {provider label} · {model}` (green) after the Server…
  button; amber `Model …` while no snapshot has arrived, red
  `⚙ No active provider` when the snapshot has no matching active provider.
  Clicking opens the "Model / Provider" dialog (same anchor/style as Server…).
- Dialog: one row per provider (`●` active / `○` others, hover shows whether
  the API key is configured). Clicking an inactive row sends
  `SetActiveProvider { id, expected_revision: snapshot.revision }` — the
  daemon-wide persistent switch, TUI `/provider` equivalent. Below, an
  editable Model field for the active provider (resynced when the active
  provider changes) with a "Save model" button that sends `UpsertProvider`
  (all other `ProviderConfig` fields copied through; omitted `ProviderUpdate`
  options preserve their values). Footer shows the config revision and a "↻"
  refetch. Demo mode hides the pill and the dialog.
- The connection worker already repaints on every event, so snapshots/rejections
  update the pill on arrival with no extra wakeups.

### Status
- **Implemented** (2026-09-08), code + tests.
- Tests: 6 new in `native/src/main.rs` — fetch issued once on the first
  connected tab (no double-send), broadcast snapshot populates the picker and
  resolves the fetch, rejection clears the snapshot + shows the notice + the
  drain's poll refetches, fetch abandoned when the sending tab disconnects,
  switch/save refuse without a snapshot or a connected tab (unchanged model is
  a no-op), no-op in demo mode.
- Validation (2026-09-08): `cargo test -p bone-desktop` = 51 passed, 0 failed,
  1 ignored; `cargo fmt -p bone-desktop` clean; release
  `bone-desktop` + `bone` rebuilt (`cargo build --release`).
- **Window smoke: pending** (user-owned, display machine). Suggested check:
  toolbar shows `⚙ …` after connect; open the pill → providers list; click
  another provider → row flips to `●`, pill updates; edit the model + Save
  model → "Saving model…" clears once the new snapshot lands; kill another
  client's config edit race → rejection message + refetch.

## Stage 5, slice 2 — attachments (images)

Composer accepts image attachments, sent with the prompt as `SubmitPrompt`
`images: Vec<ImageData>`.

- **Data flow:** 📎 picker or drag-drop → `Attachment { name, media_type,
  data_b64, byte_size }` → on send, `to_image_data()` maps each attachment to
  `bone_protocol::ImageData { name, media_type, data_b64, width: None,
  height: None, sha256: None }` → cleared with the composer on successful
  send. `Attachment::from_file` rejects non-image extensions and files over
  15 MB (`MAX_ATTACHMENT_BYTES`), surfacing the reason in `state.last_error`.
- **Supported extensions:** png, jpeg/jpg, webp, gif (case-insensitive).
- **UI:** chips row above the editor — one `🖼 name (KB)` chip per attachment
  with a removable `✕`; the send-enable rule is now *connected && ready &&
  !busy && (non-empty text OR non-empty attachments)*, so an image with empty
  composer text is sendable. Editor hint updated to
  `Message (Ctrl+Enter to send) · 📎 or drop an image`. Drag-drop is handled
  once in `ui` via `ui.ctx().input(|i| i.raw.dropped_files.clone())` routed to
  the selected tab's `apply_drops` (per handle: `path()`/`bytes()` →
  `from_file`; failures recorded per file, valid files still attach).
- **Demo mode:** 📎 hidden and drops ignored.
- **New deps:** `base64 = "0.22"` (native only); `rfd` (slice 1) covers the
  file picker via `add_filter("Images", …)`.

### Status
- **Implemented** (2026-09-08), code + tests.
- Tests: 4 new in `native/src/main.rs` — base64 encode + extension→media_type
  mapping (round-trip decode), unknown extension / oversize rejection
  (exactly-at-cap accepted), `can_send` with attachments and empty text,
  `send_prompt` clears composer **and** attachments and sets busy.
- Validation (2026-09-08): `cargo test -p bone-desktop` = 55 passed, 0 failed,
  1 ignored; `cargo fmt -p bone-desktop` clean.
- **Window smoke: pending** (user-owned, display machine). Suggested checks:
  attach via 📎 picker → chip appears, send enabled with empty text; drag an
  image in from the file manager → chip appears; drop a non-image (e.g.
  `.txt`) → error notice, no chip; send an image to the daemon → composer and
  chip clear, busy indicator engages; oversize (>15 MB) image rejected with
  the cap message.

## Stage 5, slices 3–5 — conversation rename/delete, shortcuts, split view

The remaining three desktop-workflow slices of the current session, in order:
rename/delete of durable conversations from the sidebar, a Ctrl-shortcut set
for tab/split navigation, and a resizable split view showing a second
conversation beside the selected one.

### Slice 3 — conversation rename and delete

Protocol (protocol/src/host.rs, host control plane — no serve-path changes):
- `HostRequest::ConversationRename { id: i64, title: String }` and
  `HostRequest::ConversationDelete { id: i64 }`. Both respond with the
  refreshed `HostResponse::Conversations` list, so the sidebar re-renders from
  the daemon's authoritative data after any mutation.

Core (session_db.rs + host.rs):
- `SCHEMA_VERSION` 10 → 11: `conversations` gains a nullable `title` override
  column. `SessionDb::rename_conversation(id, title)` stores it; a blank title
  clears the override (DB level). `SessionDb::delete_conversation(id)` removes
  the conversation and all of its rows. `recent_conversations` now uses
  `COALESCE(conversations.title, <derived first-user title>)` so a stored
  override wins over the derived title.
- Host service: a blank/whitespace `ConversationRename` title is refused with
  `HostErrorCode::Invalid` ("title must not be empty"); unknown ids are
  `Invalid`. The daemon is the authority; the native client also refuses blank
  titles locally to keep the field open for editing.

Native (main.rs):
- State: `rename_target: Option<i64>`, `rename_field: String`,
  `delete_target: Option<(i64, String)>`, and `mutation_target: Option<i64>`
  (which conversation id the in-flight host request mutates, for error context).
- `request_conversation_mutation(request)`: sends through the first connected
  non-demo tab (`state.next_id()`), reusing the existing `conversations_request`
  correlation; refuses when disconnected or another request is in flight.
- `start_rename` seeds the field from the list and cancels any armed delete;
  `commit_rename` trims — blank → local notice "A conversation title cannot be
  blank." (field stays open); a refused send keeps the field open with "Cannot
  rename right now: no daemon connection or a conversation update is already in
  flight.". `start_delete` is refused while any non-demo tab has
  `conversation_id == Some(id)` → "Close the tab for that conversation before
  deleting it."; `commit_delete` takes the target first, then sends (refused
  send → notice).
- `apply_host_response`: a `Conversations` response clears `mutation_target`; an
  `Error` says "Conversation update failed: {message}" when `mutation_target`
  was set, else the existing "Conversation list unavailable: {message}".
  `poll_conversations` abandonment also clears `mutation_target`.
- Sidebar UI: each row is three-way. Renaming: inline `TextEdit` (seeded) +
  ✓/✕ buttons, with Enter/Escape firing on the frame the field loses focus
  (`lost_focus() && key_pressed`). Delete-arming: inline red
  `Delete "…"? Yes / No`. Otherwise: the usual selectable row plus ✎ (rename)
  and 🗑 (delete) buttons. Row actions are collected into locals during the
  loop and applied after it.

### Slice 4 — keyboard shortcuts

- `apply_shortcut(key, ctx) -> bool` handles one key; `handle_shortcuts(ui)`
  runs at the top of `ui()` (right after `pump_daemon`, before panels) so it
  fires even while a widget has focus. It is gated on the Ctrl modifier and
  walks `const KEYS: [egui::Key; 14]`, returning after the first handled key.
- Shortcuts: Ctrl+T (new tab + connect), Ctrl+W (close selected tab),
  Ctrl+PageUp / Ctrl+PageDown (previous/next tab with wrap), Ctrl+1…Ctrl+9
  (select tab when in range), Ctrl+\\ (toggle split).
- egui 0.36.1 note: the digit variants of `egui::Key` are `Num1`…`Num9`
  (there is no `Digit*` in 0.36), and panels are the unified `egui::Panel`
  (`.top()/.left()/.right()/.bottom()`), not the old `SidePanel`/`TopBottomPanel`
  types.

### Slice 5 — split view

- State: `split: bool`, `split_tab: usize`. Not persisted to the layout file
  (a split layout is a per-session view, like the Stage-3 omissions).
- Toolbar: a `⧉ Split` selectable button (`egui::Button::selectable`), enabled
  only with ≥2 tabs, hover "Show a second conversation beside this one
  (Ctrl+\\)"; toggling re-runs `reconcile_split()`.
- `reconcile_split()`: forces `split = false` below two tabs; clamps
  `selected` and `split_tab`; shifts `split_tab` off `selected` so one
  transcript cache never renders in two panes at once.
- `split_header(ui)`: a selectable strip of tabs for the right pane, skipping
  the left-selected tab.
- Render: while split, a right `egui::Panel::right("split-pane")` (resizable,
  default 380 px, min 220 px) is reserved and rendered first — so the left
  transcript measures its area against the remaining central space — showing
  `split_header` + `tabs[split_tab].body`; then the left pane renders
  `tabs[selected].body`. Either pane changing triggers `note_layout_change`.

### Status

- **Implemented** (2026-09-08), code + tests.
- Tests: 8 new in `native/src/main.rs` — rename commit sends and the response
  updates the list, blank title refused locally, rename/delete refused without
  a connection, rename error response reports the update failure, delete
  refused while a tab has the conversation open, delete commit updates the
  list, shortcuts (select tabs / new / close / split toggle), and
  `reconcile_split` staying valid across selection changes and tab closes.
  Protocol/core additions are covered by the existing
  `host_tests.rs`/`session_db_tests.rs` suites.
- Validation (2026-09-08): `cargo test -p bone-protocol -p bone-core` all
  green (core lib 460 passed / 0 failed); `cargo test -p bone-desktop` =
  63 passed, 0 failed, 1 ignored; `cargo fmt -p bone-desktop -p bone-core
  -p bone-protocol` clean (the five pre-existing drift files untouched);
  clippy shows no warnings in the new code (all reported lints pre-date this
  session).
- **Window smoke: pending** (user-owned, display machine). Suggested checks:
  hover a sidebar row → ✎ opens the inline field (Enter commits, Escape
  cancels; blank is refused with the field still open); 🗑 on a closed
  conversation → inline confirm deletes it; 🗑 on an open conversation →
  refusal notice; Ctrl+T / Ctrl+W / Ctrl+1–9 / Ctrl+PageUp / Ctrl+PageDown
  navigate tabs; with two tabs, ⧉ Split or Ctrl+\\ shows a second pane with
  its own tab strip (closing the second tab drops the split).

## Stage 6, slices A–B — desktop hardening and release

The final stage, scoped to what this host can actually build and test:
hardening the local-daemon connection (bounded auto-reconnect on
mid-session drops, a daemon version-mismatch notice, and detection and
respawn when the app-spawned daemon dies) plus unit tests for the
failure and recovery paths. Anything that needs another OS, packaging,
or a clean machine is documented below as blocked rather than
implemented.

### Slice A — reconnect and daemon hardening

Daemon (daemon.rs):
- `MAX_RECONNECT_ROUNDS: u32 = 5` and `RECONNECT_RETRY_DELAY_MS: u64 =
  2000`: the auto-reconnect budget and the initial delay before the
  first reconnect attempt.
- `is_mid_session_drop(reason)`: classifies a disconnect reason —
  anything other than `Connect failed*`, `Disconnected`, and
  `Connection cancelled` is a mid-session drop (a previously connected
  socket was lost) and is worth an automatic retry.
- `spawn_daemon` now returns `std::io::Result<std::process::Child>`
  instead of a bare pid, so the parent can `try_wait` on its daemon.

Native (main.rs):
- `Tab` gains `mid_drop: bool` (this tab lost a connection it already
  had) and `host_api_version: u16` (the version the daemon reported).
  In `handle_event`, the `Disconnected` arm sets `mid_drop` gated on
  prior connectivity — an initial connect failure is never treated as a
  drop — and the `Connected` arm resets both; the `FrontendState`
  runtime event records the daemon's version.
- `DesktopApp` gains `reconnect_budget: u32`,
  `daemon_child: Option<std::process::Child>` (set on a successful
  `start_local_daemon`), and `version_notice: String`.
- `poll_daemon_child(ctx)` runs before each pump: it `try_wait`s the
  spawned child. On exit it clears the child, resets the budget, marks
  every tab disconnected, then either respawns — phase Ready, so it
  calls `start_local_daemon` again (success notice: "The local daemon
  (pid {pid}) exited ({status}); a new one is starting.") — or settles
  to `Stopped` ("The local daemon (pid {pid}) exited before accepting
  connections ({status}). Use Server… to start it.") with a notice.
- Bounded reconnect semantics: a mid-session drop (or a refused/other
  failure while Ready) arms the retry when none is pending; the first
  drop allocates `MAX_RECONNECT_ROUNDS`, and each tick after the
  initial 2 s delay spends one and reconnects the affected tabs. The
  6th consecutive failure exhausts the budget → `Stopped` with "Lost
  the daemon connection and could not restore it after 5 attempts.
  Reconnect manually in the Server dialog." and a notice. A successful
  connection resets the budget and the drop flags.
- `check_host_api_versions()` runs at the tail of `drain_all`: it
  observes the first non-demo tab with a recorded version and, if it
  differs from `bone_protocol::HOST_API_VERSION`, sets an
  informational notice "Daemon host API version {v} does not match
  this client ({}); some features may not work.", rendered as an amber
  toolbar label below the red daemon notice.

### Slice B — failure/recovery tests

- 8 new tests (1 in `native/src/daemon.rs`, 7 in `native/src/main.rs`),
  plus a `spawn_dead_process` helper (a `sh -c "exit 0"` /
  `cmd /c exit 0` child that exits immediately).
- `mid_session_drop_classification`: drop-reason classification —
  mid-session reasons retry, connect-failure reasons do not.
- `mid_session_drop_drives_bounded_reconnect_rounds`: a mid-session
  drop arms the bounded retry and, with no recovery, exhausts the
  budget into `Stopped`.
- `mid_session_reconnect_restores_tabs_and_clears_budget`: a successful
  reconnect reattaches the tabs and zeroes the budget.
- `idle_ready_app_never_schedules_retries`: a Ready app with no drop
  never schedules a retry.
- `tab_mid_drop_only_when_connected_socket_is_lost`: `mid_drop` is set
  only for a tab that had a connection before the drop.
- `spawned_daemon_death_after_ready_triggers_respawn`: an app-spawned
  daemon that dies while Ready is respawned and the notice is shown.
- `spawned_daemon_death_before_ready_is_stopped`: a daemon that dies
  before accepting connections settles to `Stopped` with the "exited
  before accepting connections" message.
- `host_api_version_mismatch_surfaces_notice_and_match_clears`: a
  mismatched version surfaces the toolbar notice; a matching one
  clears it.

### Status

- **Implemented** (2026-09-08), code + tests. Changes confined to
  `native/src/daemon.rs` and `native/src/main.rs`.
- Validation (2026-09-08): `cargo test -p bone-desktop` = 71 passed,
  0 failed, 1 ignored (baseline 63; +8 new); `cargo fmt -p bone-desktop
  -p bone-core -p bone-protocol` clean (the five pre-existing drift
  files untouched). No protocol/core code changed this stage, so the
  Stage 5 numbers there stand.
- **Window smoke: pending** (user-owned, display machine). Suggested
  checks: kill the app-spawned daemon → "…a new one is starting."
  notice and the tabs reconnect; point `BONE_DESKTOP_DAEMON` at a
  broken binary → "Could not start the local daemon" / "exited before
  accepting connections" paths. Budget exhaustion and the version
  label are covered by the unit tests above.

### Blocked / out of scope for this host

- macOS/Windows runtime validation — this host is Linux-only; note the
  unix-only `process_group(0)` (the Windows branch uses
  `CREATE_NEW_PROCESS_GROUP`), and no non-Linux binary has been run
  here.
- Installers/signing/notarization — the release artifact is the plain
  release binary pair (`bone-desktop` + `bone`); no packaging, signing,
  or notarization is done or possible on this host.
- Clean-machine install — the "Could not find the `bone` daemon binary
  (set BONE_DESKTOP_DAEMON or install bone)." path is unit-tested only;
  a real first run on a fresh machine cannot be exercised here.
- Credential storage / secure transport — N/A by design: local daemon
  traffic is plaintext loopback TCP; connecting to a remote host is an
  explicit user action, and TLS/tokens are daemon-side concerns.

## Desktop UX milestone — transcript, navigation, setup, images, preferences (2026-09-08)

Consolidated UX slice completing the remaining in-scope native work: workspace
identity, safe remote-connection safeguards, transcript quality, sidebar
navigation, provider setup, image display, and client-only preferences.

### What changed

- **Workspace identity and safe controls:** the toolbar surfaces the workspace
  root; the Server dialog is the only connection surface, with disconnect/
  reconnect and explicit failure notices.
- **Secure remote-access policy:** `native/src/daemon.rs::local_endpoint`
  accepts only loopback socket targets; any non-loopback address (hostname or
  LAN IP) is rejected with "Direct remote connections are disabled: … Use an
  SSH tunnel and connect to 127.0.0.1:<forwarded-port>." No TLS/auth was
  invented — remote use is an SSH tunnel to a forwarded loopback port. No
  custom ports or remote addresses ever autostart a daemon. Covered by
  `ux_tests::remote_connections_require_a_loopback_tunnel_endpoint` and
  `daemon::tests::loopback_detection`.
- **Transcript and navigation:** rendered Markdown with images
  (`native/src/images.rs`), jump-to-latest pill while scrolled up, sidebar
  "Open conversations" above the searchable "Recent" history list
  (title filter), and split-view panes with per-pane tab headers.
- **Provider setup:** new `native/src/setup.rs` `SetupUi` renders the daemon's
  `HostRequest::Setup` snapshot as a form; Save submits the `SetupApply`
  plan through the existing correlated host API (no credentials touch the
  frontend beyond what the protocol already carries). The dialog auto-opens
  once when the daemon reports onboarding is needed and is reachable from
  "View & settings → Provider setup…". Applied results invalidate the cached
  config so the next poll refetches the revisioned snapshot.
- **Preferences:** zoom (0.75–2.0, mirrored to the state file), pane widths,
  and split state persist in the desktop layout file; keyboard shortcuts
  (Cmd+T/W, Cmd+1–9, PageUp/Down, Cmd+\) are gated while dialogs are open.
- **Protocol/core additions:** setup snapshot/action types and the correlated
  host setup API in `bone-protocol`/`bone-core` (see the Stage 5 entries);
  the native client consumes them without owning setup state.

### Validation

- `cargo test -p bone-desktop`: **106 passed, 0 failed, 1 ignored** (release
  ignored perf test included). `cargo test -p bone-protocol`: 22 passed.
  Workspace (core/TUI) suites green.
- `cargo build -p bone-desktop --release`: clean, zero warnings.
  `cargo clippy -p bone-desktop --tests`: only pre-existing warnings remain.
  `cargo fmt -p bone-desktop -- --check`: clean.
- Performance (release, headless, `native/src/perf_tests.rs`): long-history
  8 MiB warm p95 0.09 ms, 20 MiB stress 0.06 ms, 100k-line tool output 1.30 ms,
  200×2000 tool outputs 0.05 ms — all under the 16.7 ms P95 budget. The
  sampling window skips the first three frames: frame 2 in a headless run
  absorbs a one-time font-atlas rebuild (~24 ms) that a running app never
  repeats, so measuring it would misrepresent steady-state cost.

### Visual verification

- Live window smoke **not run this pass (blocked, user-approved skip)**: the
  display GPU (GPU1) had ~0.6 GiB free because `llama-server` held ~20.6 GiB,
  so a new wgpu device failed `RequestDeviceError(Device(OutOfMemory))`
  regardless of `WGPU_ADAPTER`/`WGPU_MSAA_SAMPLES` selection. The pre-existing
  old-build window was left untouched per the user. Verified instead by the
  106 unit/integration tests (setup, sidebar, split, reconnect, remote
  policy), the headless release perf test, and the Stage 2/3/Layer smoke
  evidence recorded above. The Layer-1/2/Layer-A/B window runbooks remain the
  user-run path when VRAM is available.

### Remaining gaps (explicit)

- End-to-end tool/approval demo in the window (mock provider is text-only).
- macOS/Windows runtime validation; installers/signing; clean-machine first
  run (Linux-only host).
- Syntax highlighting, variable-height virtualization, and bounded parse-cache
  eviction remain deferred; measured warm frames currently sit well under
  budget, so none blocks this milestone.
