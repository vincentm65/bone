# Architecture

```text
            ┌──────────── bone (binary) ────────────┐
            │  CLI: TUI / --headless / --listen     │
            └───────┬──────────────────┬────────────┘
                    │                  │
     bone-tui ──────┘                  └────── bone-server
  (fullscreen UI, TUI Lua)               (connections, handshake,
            │                             event fan-out)
     bone-client                                 │
  (typed requests, events)                   bone-core
            │                     (sessions, agent loop, provider,
            └──── bone-proto ────  tools, hooks, core Lua)
          (JSON-RPC types, codec,
           transports)              bone-lua: shared Lua setup and
                                    the embedded runtime/ directory
```

| Crate | Owns |
|---|---|
| `bone-proto` | the only contract: message envelope, typed methods/events, NDJSON codec, `Connection` and transports |
| `bone-core` | everything authoritative: sessions (JSONL files), the agent loop, the OpenAI-compatible provider, built-in tools, hooks and `bone.ask`, `core.lua` |
| `bone-server` | hosts a core for any number of connections: stdio, Unix socket, in-process |
| `bone-client` | request/response matching and the event stream, for any transport |
| `bone-tui` | the UI: the chat view, prompt, `/` commands, popups, keys, rendering, `tui.lua` |
| `bone-lua` | shared Lua setup: `require` paths, plugins, JSON, helpers, the built-in runtime files |
| `bone` | the command line |

## Rules

- **The core never knows about UIs.** It takes requests and emits events. The TUI uses the same API as any other client, even when the core runs in the same process.
- **The core is authoritative.** The TUI keeps only view state (scroll position, prompt text, open popups). Transcripts, turns and open questions live in the core, which is why several UIs stay in sync.
- **Two Lua states, never shared.** Core Lua runs on its own thread (`bone-lua`); tools, hooks and the system prompt reach it through a job channel, so a slow Lua tool only blocks other Lua work. TUI Lua runs on the UI thread. Each API call gets the live UI state through a scoped function for the duration of the call, so Lua can't hold dangling references and nested calls are safe.
- **Rust draws, Lua decides.** The TUI's Rust code holds the session as data (items: user, reasoning, assistant, tool, notice), wraps text, caches rendered lines per item, scrolls and paints. Every look-and-feel decision (spacing, prefixes, colors, Markdown, tool views, regions around the chat, statusline) is a Lua function, and an item is only re-rendered when it, the width, or the views change. Without Lua, Rust draws plain text.
- **Defaults are Lua.** Keys, views, the statusline, the divider and colorschemes are files in `runtime/`, compiled into the binary. `~/.bone/runtime/<same path>` replaces any of them.

## A turn

1. The TUI sends `turn/start`. The core marks the session busy and returns; the agent task runs `turn_start` hooks, saves the user message and emits `turn/started`.
2. It builds the request: the system prompt (Lua may supply it), then the transcript, passed through `request` hooks. It streams the completion, emitting `message/delta`, runs `message` hooks on the reply and saves it as the assistant message (`message/completed`).
3. For each tool call, in order: `tool_call` hooks (which may deny or rewrite the call, or ask the user; asking for approval is just the opt-in `approve` plugin doing that), the tool itself (built-in or Lua), then `tool_result` hooks. Each result is saved and emitted (`tool/finished`).
4. The loop goes back to step 2 until the model answers without tools. Cancellation can land at any point: partial text is kept, and every unanswered tool call gets a "cancelled" result so the transcript stays valid, and any open `bone.ask` gets `nil`. `turn_end` hooks run last.

## Storage

`~/.bone/sessions/<uuid-v7>.jsonl`: one header line, then one line per message, appended as they happen. A torn last line (from a crash) is dropped when the session is next loaded.

## Tests

- Unit tests next to the code: the agent loop against a scripted provider, tools, Lua, rendering and layout.
- `bone-proto/tests/golden.rs`: the wire format of every method and event.
- `bone-server/tests`: client ↔ server ↔ core, including the HTTP provider against a fake SSE server.
- `bone/tests/headless.rs`: the real binary over stdio and sockets.
- `bone/tests/tui.rs`: the TUI driven headlessly (`bone_tui::Headless`) against a real core and a fake model, with screen snapshots.
