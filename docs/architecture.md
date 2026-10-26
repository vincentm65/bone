# Architecture

```text
            ┌────────────── bone (binary) ──────────────┐
            │ CLI: TUI / --gui / --headless / --listen  │
            └──┬──────────────┬──────────────────┬──────┘
               │              │                  │
     bone-tui ─┘   bone-gui ──┘                  └── bone-server
  (terminal, ratatui)  (desktop, egui)          (connections, handshake,
               │              │                   event fan-out)
               └── bone-ui ───┘                          │
        (shared UI state, Lua UI API,                bone-core
         the Host trait, layout, keys)    (sessions, agent loop, provider,
                      │                    tools, hooks, core Lua)
                 bone-client                             │
           (typed requests, events)                      │
                      └──────────── bone-proto ──────────┘
                           (JSON-RPC types, codec, transports)

     bone-lua: shared Lua setup and the embedded runtime/ directory
```

| Crate | Owns |
|---|---|
| `bone-proto` | the only contract: message envelope, typed methods/events, NDJSON codec, `Connection` and transports |
| `bone-core` | everything authoritative: sessions (JSONL files), the agent loop, built-in and Lua providers, tools, hooks and `bone.ask`, `core.lua` |
| `bone-server` | hosts a core for any number of connections: stdio, Unix socket, Windows named pipe, in-process |
| `bone-client` | request/response matching and the event stream, for any transport |
| `bone-ui` | what both frontends share and is not drawing: chat items, composer, keymaps, options, popups, panels, the TUI's layout solver (in character cells), Markdown, themes, and the Lua UI API (`bone.ui`, `runtime/ui/`) behind the `Host` trait |
| `bone-tui` | the terminal frontend: rendering with ratatui, terminal keys and mouse, terminal views and jobs, `tui.lua` and `bone.tui` |
| `bone-gui` | the desktop frontend (`bone --gui`, the `gui` feature): an egui/eframe window that draws a tree of nodes from Lua (`node.rs`, `flex.rs`, `nodeview/`: `mod.rs`, `paint.rs`, `list.rs`, `focus.rs`, `ops.rs`), native text editing, `gui.lua` and `bone.gui`; a headless mode for tests |
| `bone-media` | bounded image decoding and PNG normalization, shared by the core and both frontends |
| `bone-lua` | shared Lua setup: `require` paths, plugins, JSON, helpers, the built-in runtime files |
| `bone` | the command line |

## Startup

`bone` and `bone --gui` both call `attach`. With no `--connect`, and no private core override, they use the default socket (`$XDG_RUNTIME_DIR/bone3/bone.sock`, falling back to the temporary directory's `bone3-$USER/bone.sock`, or `\\.\pipe\bone3-USERNAME` on Windows): connect if a server is already listening, otherwise bind it and host the core in this process. That hosted socket lives only as long as the UI. The TUI serves it until the TUI exits; the GUI's task is aborted when the window closes. Dropping the listener removes the socket. Another frontend can attach for that lifetime.

An explicit `--connect [PATH]` (the default path when `PATH` is omitted) never starts a core. If the connect fails, that is an error. `core.lua` is loaded only after the attach-or-host choice, so a process that attaches does not apply its own core config.

A nonempty `BONE_CONFIG_DIR`, `BONE_DATA_DIR`, `BONE_MODEL`, `BONE_BASE_URL`, `BONE_API_KEY`, `BONE_REASONING_EFFORT`, `BONE_SYSTEM_PROMPT`, `BONE_APPROVAL` or `BONE_DEBUG_HTTP` skips the default socket. The core stays in-process and is not served. Those variables do not turn an explicit `--connect` into a host.

`bone --headless --listen [PATH]` is a different lifetime: it binds and serves until SIGINT or SIGTERM, with no UI. `--listen` without `--headless` is an error. `--headless` alone serves stdin/stdout.

## Rules

- **The core never knows about UIs.** It takes requests and emits events. The TUI uses the same API as any other client, even when the core runs in the same process.
- **The core is authoritative.** The TUI keeps only view state (scroll position, prompt text, open popups). Transcripts, turns and open questions live in the core, which is why several UIs stay in sync.
- **Lua states are never shared.** Core Lua runs on its own thread (`bone-lua`); tools, hooks and the system prompt reach it through a job channel, so a slow Lua tool only blocks other Lua work. Each frontend's Lua runs on its UI thread. Each API call gets the live UI state through a scoped function for the duration of the call, so Lua can't hold dangling references and nested calls are safe.
- **Rust draws, Lua decides.** The TUI's Rust code holds the session as data (items: user, reasoning, assistant, tool, notice), wraps text, caches rendered lines per item, scrolls and paints. Every look-and-feel decision (spacing, prefixes, colors, Markdown, tool views, regions around the chat, statusline) is a Lua function, and an item is only re-rendered when it, the width, or the views change. Without Lua, Rust draws plain text.
- **One UI API, two frontends.** `bone_ui::lua` implements the shared ops (options, events, commands, keymaps, themes, views, layout, windows, panels, input, plugins) once, against the `Host` trait that `bone_tui::App` and `bone_gui::Gui` implement: accessors for their state, and services such as callbacks, requests and the clipboard. Each frontend adds its own ops (`bone.tui`, `bone.gui`) and raises a clear error for ones it lacks. The two frontends share behavior, not a view: the TUI lays out and draws in character cells (`bone.ui.layout`, regions, `lines`), and the GUI draws a CSS-like tree of nodes Lua returns from `bone.gui.view` (flex boxes in pixels, text, a native prompt field, scrolling lists). A plugin's `ui.lua` (commands, keys, options, events, data, `bone.ui.popup`) runs in both, and its `tui.lua` or `gui.lua` only in that frontend.
- **Defaults are Lua.** Keys, views, the statusline, the divider and colorschemes are files in `runtime/`, compiled into the binary. A frontend tries its active library first (`~/.bone/libraries/<name>/`, from `/tui` or `/gui`), then `~/.bone/runtime/<same path>`, then the built-in file. `bone.builtin` always reads the built-in copy. The core does not use a UI library; only a runtime override replaces its files.

## UI layout and providers

The TUI places chats in a character-cell layout (`bone.ui.layout`). Leaves named `chat`, `prompt`, `divider` and `statusline` are the current chat; `chat:2`, `prompt:2` and `statusline:2` select another by its zero-based number or session ID. Each chat keeps its own scroll position and draft, including attachments; prompt and statusline callbacks receive that chat's context. Popups overlay the layout. A docked Lua panel, or a `panel:<id>` leaf, can take focus.

The GUI does not use those leaves. `bone.gui.view` returns a tree of nodes (flex boxes in pixels). The standard window is a sidebar and one chat column (the chat list, composer, tray and status bar); Lua can place more than one column, and each chat still has its own scroll and draft. Panels dock as nodes (`node = fn` on `bone.ui.panel`), not as cell leaves. Popups are nodes, or monospace lines when a window was written with `lines`.

The tray, in both frontends, draws core-managed shell processes from `process/changed` and `process/read`, and sends their size with `process/resize`. Those programs run where the core runs. `/term` is a local pseudo-terminal instead (`bone.job` with `pty`, Unix only), so the program runs on the frontend's machine even when the core is remote. The GUI ships `/term`. On the TUI it comes from the optional **term** plugin. A plugin command of the same name replaces the GUI's.

Built-in providers support Chat Completions (`completions`), OpenAI Responses (`responses`) and Anthropic Messages (`anthropic`); Lua can register other provider types. An explicit `api` overrides automatic selection: `api.openai.com` uses Responses, `api.anthropic.com` uses Anthropic, and a `claude-` model with no URL uses Anthropic's default URL; other endpoints use Chat Completions. A session saves its provider/model selection, initially locked on its first model call. `settings/set` with `session_id` can update that session's selection without moving other sessions; `model/list` with the same ID reports it. Request hooks may override the provider/model for one call without changing the saved selection.

## A turn

1. The TUI sends `turn/start`. The core marks the session busy and returns; the agent task runs `turn_start` hooks, saves the user message and emits `turn/started`.
2. It builds the request: the system prompt (Lua may supply it), then the session context (a summary plus the remaining transcript after compaction), passed through `context` hooks, then `request` hooks with the available tools. These hooks shape the call, not the stored transcript. It streams the completion, emitting `message/delta`, runs `message` hooks on the reply and saves it as the assistant message (`message/completed`), including provider `reasoning_state` for replay on compatible later calls.
3. For each tool call, in order (consecutive read-only calls run at the same time; their results are still saved in order): `tool_call` hooks (which may deny or rewrite the call, or ask the user; approval can be implemented with `bone.ask` in a hook), the tool itself (built-in or Lua), then `tool_result` hooks. Each result is saved and emitted (`tool/finished`).
4. The loop goes back to step 2 until the model answers without tools. Messages a client steers into the running turn (`turn/steer`) are added before the next model call, and one that arrives during the final answer gets its own step. Cancellation can land at any point: partial text is kept, and every unanswered tool call gets a "cancelled" result so the transcript stays valid, and any open `bone.ask` gets `nil`. `turn_end` hooks run last. Unconsumed steer messages become queued next turns, including after cancellation; completed and failed turns advance the queue, while cancellation normally pauses it. A new message submitted after cancellation, while the turn winds down, is placed ahead of older queued items and keeps the queue running.

## Storage

`~/.bone/sessions/<uuid-v7>.jsonl`: one header line, then one line per message, appended as they happen, each with the time it was written (`at`). Compaction (`compact.rs`) appends a `summary` line: the summary the model gets from then on in place of the transcript's first N messages, which stay in the file and in what clients see (`null` clears it). `bone.session.compact` appends a checkpoint that replaces the transcript instead; a `usage` line after each model call says what it used (provider, model, tokens). Model calls outside turns are counted too: Lua's in the session they were made for, a client's (`model/complete`) in `~/.bone/usage.jsonl`. Calls a Lua provider makes to serve a request are not, since the provider's own result counts them. A `model` line persists the session's provider/model selection; assistant messages retain opaque reasoning state as well as visible reasoning text. A torn last line (from a crash) is dropped when the session is next loaded.

Each session's `<id>.queue.json` saves queued messages and attachment references. Loading a nonempty queue pauses it until `queue/resume` or a new `queue/add`. Queue save failures are reported in `queue/changed.error`; accepted in-memory changes still apply and can run, but may be lost on restart.

`~/.bone/index.db` is a SQLite index over those files, for `store/query`: `sessions` (`root` is the top-level session a subagent works for, null for others), `messages` (role, time, size), `search` (FTS5 over user and assistant text), `usage` (with `source`: `turn`, `lua` or `client`; `session_id` is `''` for none) and `tool_calls` (name, error, output size). The files stay the record: the index reads what was appended to each since it last looked, during turns and at startup, so it can be deleted and comes back.

Image uploads use `attachment/upload` regardless of transport. The core normalizes PNG/JPEG/WebP to bounded PNGs and atomically writes content-addressed files in `<data_dir>/attachments/`. Messages, queues, forks and events hold metadata and IDs; just before a model call the core verifies and hydrates the files. `read_file` image results use the same store and retain references on tool messages. Built-in providers send API-specific multimodal text/image content; Chat Completions and Responses place tool images after all corresponding text tool replies, while Anthropic includes them in tool results. Lua providers receive base64 PNG data on user and tool messages. Compaction estimates image cost, includes images when the summary model supports them and retains the original transcript. Files are deduplicated but currently retained indefinitely.

The TUI reads native clipboard pixels in a short-lived helper process; a ten-second timeout kills and reaps a stuck helper so the next paste can recover. File reads are asynchronous, and the core handles file decoding and normalization. Composer state owns attachments, pending reads, per-session drafts and history; draft generations keep late reads from reappearing after a clear or attaching to another session. Image-only drafts submit, queue and edit through the same paths as text.

## Tests

- Unit tests next to the code: the agent loop against a scripted provider, tools, Lua, rendering and layout.
- `bone-proto/tests/golden.rs`: the wire format of every method and event.
- `bone-server/tests`: client ↔ server ↔ core, including the HTTP provider against a fake SSE server.
- `bone/tests/headless.rs`: the real binary over stdio and sockets.
- `bone/tests/tui.rs`: the standard TUI driven headlessly (`bone_tui::Headless`) against a real core and a fake model, with screen and interaction assertions.
- `tests/ui/*.lua`: conformance scripts for the shared Lua UI API, run in both frontends (`bone/tests/conformance.rs`, `bone-gui/tests/conformance.rs`); the four about the TUI's cell layout (`frontend`, `layout`, `panels`, `scrolling`) run only in the TUI.
- `bone-gui/tests`: the GUI driven headlessly (`bone_gui::Headless`, egui without a window): Lua hosting, the node view (layout, scrolling, lists, painted output), keys, clicks, resizing and HiDPI, and several clients (TUI and GUI) on one server, including a server restart.
- `bone/tests/images.rs`: image upload, model payloads, persistence/restart, queue editing, draft races, compaction, and optional native clipboard through a separate headless process.
