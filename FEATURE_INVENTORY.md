# Bone v2.2.9 — Feature Inventory

> Organized by logical subsystem for code review.

---

## 1. Runtime Core

### 1.1 Agent Loop / Driver (`core/src/runtime/driver.rs`, `core/src/agent.rs`)

- `Driver` struct — single agent loop owning provider, tools, extensions, session sink
- `run_to_outcome()` — drives provider → tool calls → persistence; returns `DriverOutcome`
- Panic recovery — `catch_unwind` + pre-turn snapshot; returns pre-turn conversation on panic
- Cancel propagation — `select!` with 25ms poll on `AtomicBool`; wired to `cancel_token`
- Sub-agent depth cap — `SUBAGENT_MAX_TURNS = 30` for depth > 0
- Turn messages — `apply_turn_messages()` injects system-reminders into tool results or trailing user messages
- `DriverOutcome` — result, tools, transcript, token_stats, persist_messages, usage
- Headless agent — `AgentRequest`, `AgentResponse`, `run_agent()` wrapper (`core/src/agent.rs`)
- Context estimation — `estimate_context_chars()`, `estimate_tokens()`
- JSONL events — `emit_event()` for `--events` flag

### 1.2 Runtime Protocol (`core/src/runtime/`)

- Events/Commands — `RuntimeEvent` (core→frontend), `RuntimeCommand` (frontend→core)
- Key registry — `KeyReplyRegistry` routes key replies by id, pauses work timer
- Approval registry — `ApprovalReplyRegistry` routes approval decisions by id
- Channel gate — `ChannelApprovalGate` emits `ApprovalRequest`, awaits reply, falls back on detach
- Runtime session — `RuntimeSession` owns transcript, token stats, tool handler, SQLite
- Session init — `init_db()` resumes latest conversation, recycles empty, mints new
- Outcome folding — `apply_outcome()` adopts state, persists in one WAL transaction
- Connection trait — `RuntimeConn` pumps commands/events at transport edge
- Local conn — `LocalConn` in-process, owns Driver future, no spawn
- Socket conn — `SocketConn<R>` remote, writer task, `None` = connection closed
- View model — `ViewModel` + `ViewDiff` for frontend deltas
- Work timer — tracks idle/active time, pausable for key/approval waits

### 1.3 Session Persistence (`core/src/session_db.rs`, `core/src/session_sink.rs`)

- Session DB — SQLite: conversations, messages, usage, compaction checkpoint
- Session sink trait — `SessionSink` injectable for testability
- Session writer — `SessionWriter` with `Mutex<Option<SessionDb>>`, failure counter
- Conversation resume — resumes latest on boot, recycles trailing empty
- Usage tracking — per-provider: prompt/completion/cached tokens, cost, request count
- Turn persistence — `append_turn_with_checkpoint()` atomic batch write of messages + usage

---

## 2. LLM Provider Layer (`core/src/llm/`)

- Provider trait — `LlmProvider`: streaming, chat, tool definitions
- Provider factory — factory-based instantiation (OpenAI-compatible, Codex)
- Chat types — `ChatMessage`, `ChatRole`, `ToolCall`, `ToolResult`, `ImageData`
- Streaming events — `ChatEvent`: TextDelta, ReasoningDelta, EncryptedReasoning, ToolCall, TokenUsage
- Error taxonomy — `LlmErrorKind`: Connection, Timeout, Auth, RateLimit, Server, Parse, Config
- Token tracking — `TokenStats`: cumulative sent/received/cached/cost
- System prompt — dynamic assembly: cwd, memory blocks, scoped project memory (1600-char truncate)
- Output items — `OutputItem`, `Reasoning`, `ReasoningItem`

---

## 3. Tool System (`core/src/tools/`)

### 3.1 Core

- Tool trait — `Tool`: definition(), execute(), execute_output(), execute_output_live()
- Registry — `ToolRegistry`: HashMap dispatch, sorted definitions
- Handler — `ToolHandler`: enabled set, state_map, display/safety maps, cancel_token
- Approval gate — `ApprovalGate` trait: async decision seam
- Auto-approval — `AutoApprovalGate`: pure `decide_call(blocked, auto_allows)`
- Escalating gate — `EscalatingGate`: forwards would-be-denied to parent for interactive approval
- Approval mode — `Safe/Danger` with `SharedApprovalMode` (atomic for live toggle)
- Command policy — safety classification for tool calls
- State map — `ToolStateMap`: per-tool in-memory state (source → sub_key → value)
- Display config — `ToolDisplayConfig`: args, template, show/show_result/eager flags
- Live events — `ToolLiveEvent::Key`: blocking key request

### 3.2 Built-in Tools

- Shell tool — streaming, timeouts, setsid detachment, output truncation
- Read file — file reading
- Write file — file writing
- Edit file — file editing
- Atomic writes — temp file + rename, crash-safe

---

## 4. Lua Extension System (`core/src/ext/`)

### 4.1 Boot & Engine

- Loader — `boot()`: VM → init.lua → tools/commands → snapshots
- Engine — `create_engine()`: sandbox, bone table, cjson, package.path
- Sandbox — blocks os.*, io.*, dofile, loadlib; only ctx APIs for I/O
- Log table — `bone.log.{info,warn,error}` writes to bone.log file
- Print override — global `print` → lua_log() (TUI-aware)
- Banner — default `bone.banner()` with term-width box drawing
- Init choices — `populated_init_lua()` (banner + subagent), `blank_init_lua()`

### 4.2 Registration & Ops

- Tool registration — `bone.register_tool(table)` → `bone._tools` array
- Subagent registration — `bone.register_subagent(table)` → `bone._subagents` table
- Command registration — `bone.register_command(name, def)` → `bone._commands` array
- Event hooks — `bone.on(event, handler)`: 10 pre-seeded + custom events
- Plugin API — `bone.plugin.{load,install,remove,list,update}`: GitHub clone + symlink

### 4.3 Lua Tool Implementation

- LuaTool impl — implements `Tool` trait; run_execute() with Lua mutex discipline
- Tool output parsing — JSON envelope → content/state/pane/images; plain text fallback
- Stateful tools — `stateful = true` opt-in; host serializes calls, threads state
- Images support — `images: [{media_type, data}]` → ImageData for vision models

### 4.4 Context & API

- Ctx table — ctx.shell/read_file/write_file delegation with policy
- Shared state — `ctx.state`: process-wide HashMap across tools/commands/hooks
- Usage context — `UsageContext`: sent/received/cached/cost/tool_count by_provider
- Runtime events — `ctx.on_runtime_event()`: maps 15+ RuntimeEvent variants to Lua
- Agent API — `ctx.agent.{run,spawn}`: sub-agent dispatch with cancellation
- YAML conversion — yaml_to_lua(): serde_yaml → Lua bridge
- Boot options — `BootOptions`: depth, headless, model, provider, tool_allowlist
- Extension manager — `ExtensionManager`: Lua VM, snapshots, commands, shared UI
- Event dispatch — `dispatch_event()`: iterates handlers, blockable events
- Lua return actions — conversation_replace/load, system_prompt_append, turn_message, tool_filter
- Config actions — Apply, ReloadTools, SwitchProvider; wire round-trip via protocol

### 4.5 UI & Runtime API

- UI API — `bone.api.ui.{open_float, set_lines, close, set_statusline, set_highlight, term_width}`
- UI state — `SharedUi` = Arc<Mutex<UiState>>: ViewModel + pending ViewDiff drain
- Runtime API — `bone.api.{autocmd, emit, keymap, config, submit}`: Phase 6 always-available
- Inbox — process-global VecDeque; Lua queues prompts for frontend
- Job registry — `JobRegistry`: background sub-agent tasks, concurrency, FIFO
- Job types — JobStatus: Queued/Running/Done/Error; Job with transcript, trace, cancel
- Job injection — `format_results_for_injection()`: auto-delivers finished jobs to model
- Job spill — `spill_result()`: results > 16k chars to temp file
- Status glyphs — ✓ ✗ ◑ ⧗

### 4.6 Snapshots

- Config snapshot — approval_mode, status_show, spinners, texts
- Theme snapshot — palette, shell, syntax, highlights, 50+ UI color fields
- Keymap snapshot — normal/insert mode bindings
- Spinner presets — SpinnerPreset, TextPreset parsed from ui.spinners module

### 4.7 Catalog & Seeding

- Catalog client — GitHub raw fetch; sha256 integrity; install/remove; 6h background refresh
- Update detection — `needs_update()` compares on-disk sha256 vs catalog
- Seed system — `seed_default_lua_{tools,commands,libs}` with onboarding selection filtering
- Refresh logic — `should_refresh_seeded_lua()` detects stale API usage
- Description extraction — prefers description field, falls back to -- comment

---

## 5. RPC / Daemon (`core/src/rpc/`)

- Codec — newline-delimited JSON framing
- Hub — fan-out events, merge commands; nvim-embed style
- HubPublisher — event-only half; drops command sender
- Session manager — `run_session_manager()`: one actor per conversation
- Managed runtime — per-conversation hub, initial event injection
- Remote client — `RemoteClient`: connect/disconnect, submit, cancel, approval/key reply
- Serve connection — `serve_connection()`: glue TCP stream to Hub
- Run daemon — `run_daemon()`: headless daemon loop with JSONL stdout

---

## 6. Configuration (`core/src/config/`)

- Bone dir — $XDG_CONFIG_HOME/bone-rust or ~/.bone-rust
- YAML pages — `CustomConfigs`: distributed config/*.yaml (general, tools, providers, status, commands)
- Field types — String, Number, Bool, Enum, Provider
- Deny-list pages — tools/commands stored as disabled list
- Provider config — `ProviderEntry`: label, base_url, model, api_key, endpoint, handler
- User config — `UserConfig`: approval_mode, enabled_tools, status_show, spinner settings
- Onboarding — `needs_onboarding()`, `apply_onboarding()`: .setup.json marker
- API key warning — `warn_if_no_api_key_for()`: checks providers, Codex auth

---

## 7. TUI (Terminal UI)

- Terminal UI — ratatui + crossterm, raw mode, status bar
- Input handling — normal/insert mode keymaps from bone.keymap snapshot
- Rendering — drains ViewDiff from SharedUi, renders floats/panes/statusline
- Approval UI — interactive prompts with safe/danger color coding
- Key input — ctx.ui.key() blocks tool execution, waits via KeyReplyRegistry
- Spinner — configurable styles, rotating thinking text, custom phrases
- Status bar — toggleable: model, approval, tokens (curr/in/out/total), queue, spinner, timer
- Config UI — /config edits CustomConfigs pages in-place
- History — /history loads past conversations via conversation_load
- Compaction — /compact summarizes, replaces transcript via conversation_replace
- Memory — /memory quiet global/project memory update
- Catalog — /catalog browse/install/remove catalog tools/commands
- Setup — /setup re-seed config, tools, commands
- Provider switching — config.switch_provider action

---

## 8. Web UI (`webui/`)

- SPA — flat index.html + styles.css + app.js, no build step
- Bridge — bridge.mjs connects to daemon on 127.0.0.1:7878, spawns if needed
- Server — bone web serves http://localhost:4577
- Chronological ordering — messages appended in send order
- Scroll management — no scroll-jacking on new messages
- Prompt dial — input with configurable behavior

---

## 9. Protocol Types (`protocol/`)

- Wire types — RuntimeEvent, RuntimeCommand, SessionSnapshot, CommandAction
- Chat types — ChatMessage, ChatRole, ToolCall, ToolResult, ToolDefinition
- View types — PaneContent, PaneLineSpec, PaneSpanSpec, ViewDiff, Component
- Input types — KeyEvent
- Token constants — CHARS_PER_TOKEN
- Provider context — UsageProviderContext
- Call outcome — CallOutcome: Approve, Denied, Blocked
- Conversation load — ConversationLoad payload

---

## Key Architectural Decisions

1. Neovim-style split: TUI/web are thin clients; bone-core owns all runtime state
2. Driver as single source: one Driver loop serves headless, TUI, and daemon paths
3. Lua as extension surface: tools, commands, events, plugins all register through Lua
4. Injectable session sink: SessionSink trait decouples persistence from agent loop
5. Atomic approval mode: SharedApprovalMode allows live toggle mid-turn
6. Panic recovery: catch_unwind + pre-turn snapshot prevents conversation loss
7. Distributed config: YAML pages in config/ instead of single monolithic file
8. Background job injection: finished sub-agent results auto-delivered to model
9. Catalog updates: network-fetched, sha256-verified, user-applied (never auto-installed)
10. Sandboxed Lua: blocks os.*, io.*, dofile, loadlib; only ctx APIs for I/O
