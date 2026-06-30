# bone studio

A simple, elegant web front-end for [bone](../) — modeled after the Codex / Cursor
chat experience. It connects to a `bone serve` daemon over bone's runtime protocol
and surfaces the agent's full feature set: streaming responses, reasoning,
live tool calls, tool-approval prompts, slash commands, Safe/Danger mode,
token/cost usage, and conversation history.

The layout is three columns — **sidebar │ chat │ canvas**. The canvas is a
resizable split-screen panel that opens automatically as the agent works:
`write_file` of a Markdown plan renders live as a document, other writes show
as a file view, and `edit_file` renders a colour-coded diff parsed from the
result. Each touched file becomes a tab so you can step back through them; drag
the divider to resize, or toggle the panel from the header.

```
  browser ──HTTP / SSE──▶ bridge.mjs ──TCP (newline-JSON)──▶ bone serve
```

The bridge is **zero-dependency** Node (built-ins only). The browser can't open a
raw TCP socket, so the bridge proxies:

- `GET /api/events` — Server-Sent Events; opens a fresh daemon connection and
  streams every `RuntimeEvent`.
- `POST /api/command` — one `RuntimeCommand`, written to the daemon socket.

Each browser tab has its own TCP attachment. `bone serve` routes that attachment
to a conversation actor keyed by the existing SQLite `conversations.id`:

- different conversations can run model turns concurrently;
- clients viewing the same conversation share one actor and one event stream;
- loading or creating a chat switches only the requesting tab;
- cancellation and approvals are scoped to the attached conversation.

The selected conversation is stored in tab-local `sessionStorage` and restored
after a bridge or daemon reconnect. No database migration or session-id column
is required.

## Run

```bash
node webui/bridge.mjs
# then open http://localhost:4577
```

If no daemon is listening on `127.0.0.1:7878`, the bridge starts one for you
(`target/release/bone serve`, falling back to `target/debug` or `cargo run`).

### Environment

| var         | default          | meaning                          |
| ----------- | ---------------- | -------------------------------- |
| `PORT`      | `4577`           | HTTP port for the UI             |
| `BONE_ADDR` | `127.0.0.1:7878` | address of the `bone serve` daemon |
| `BONE_BIN`  | _(auto-detect)_  | path to the `bone` binary to spawn |

There are **no typed slash commands** — every affordance is a UI element:
a chat sidebar (history), a tabbed Settings modal (config/themes/usage), and a
model picker. The sidebar, provider list, and config toggles are read from bone's
own local data (`conversations.db`, `providers.yaml`, `general.yaml`, `tools.yaml`)
via extra bridge endpoints, since the runtime protocol has no list/config commands.

## Features mapped to the protocol

| UI                              | wire / source                                          |
| ------------------------------- | ------------------------------------------------------ |
| Streaming reply + caret         | `text_delta` → `finished`                              |
| Collapsible "Thinking"          | `reasoning_delta`                                      |
| Rich tool cards (icon/verb/args)| `tool_call` / `tool_result`                            |
| **Inline** approval cards       | `approval_request` → `approval_reply`                  |
| Chat sidebar + open             | `GET /api/conversations` → `load_conversation`         |
| Model picker / switch           | `GET /api/providers` → `switch_provider`               |
| Settings → Behavior, Tools      | `GET/POST /api/config` (+ `reload_extensions`)         |
| Settings → Display (client)     | `localStorage` (thinking / expand tools / meter)       |
| Safe / Danger toggle            | `set_approval_mode`                                    |
| Token + cost meter              | `state_snapshot` / `token_usage`                       |
| New chat / Stop                 | `new_conversation` / `cancel`                          |
| History replay on attach        | `conversation_loaded`                                  |
| **Canvas** doc / diff viewer    | `tool_call` (write_file content) / `tool_result` (edit_file diff) |
| Live theme accent               | `frontend_state.theme` / `view_diff` (`set_highlight`) |

### Bridge endpoints

| route                  | purpose                                              |
| ---------------------- | ---------------------------------------------------- |
| `GET /api/events`      | SSE stream of `RuntimeEvent`s                        |
| `POST /api/command`    | one `RuntimeCommand` to the daemon                   |
| `GET /api/conversations` | recent chats from `conversations.db`               |
| `GET /api/providers`   | provider list from `providers.yaml`                  |
| `GET/POST /api/config` | read / write `general.yaml` + `tools.yaml` toggles   |
