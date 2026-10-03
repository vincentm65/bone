# The bone protocol

Everything that talks to the bone core (the TUI, scripts, editors) uses this
API. It is JSON-RPC 2.0, one JSON message per line (newline-delimited JSON).

The typed definitions in `crates/bone-proto/src/methods.rs` and `types.rs` are the source of truth. Exact wire examples for every method and event are in `crates/bone-proto/tests/golden/`; tests fail if the wire format changes.

## Connecting

| Transport | How |
|---|---|
| stdio | start `bone --headless`, write requests to its stdin, read its stdout |
| Unix socket | `bone --headless --listen [PATH]`, connect to `PATH` (default `$XDG_RUNTIME_DIR/bone3/bone.sock`, mode 0600) |
| in-process | Rust only: `Server::connect_in_process()` |

One socket server serves many clients. Each one sees every event, so several UIs can watch and drive the same session.

From the shell:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":0,"client_name":"sh"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"session/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown"}' | bone --headless
```

## Handshake

The first request must be `initialize`. Anything else gets `-32002`. The client sends its `protocol_version`, and a different version gets `-32003` with the server's version in `error.data`. The connection is usable after a failed handshake, so the client can retry. The protocol is version `0` until it is declared stable; until then, client and server must match exactly.

Events are only sent to connections that have completed the handshake.

## Methods

| Method | Params | Result |
|---|---|---|
| `initialize` | `{ protocol_version, client_name }` | `{ protocol_version, server_name, server_version }` |
| `shutdown` | `{}` | `null`; then the server closes this connection |
| `echo` | `{ text }` | `{ text }`, plus an `echoed` event (for testing) |
| `session/create` | `{ cwd? }` | `SessionInfo`. `cwd` defaults to the server's directory, so clients should send their own |
| `session/list` | `{}` | `[SessionInfo]`, newest first |
| `session/messages` | `{ session_id }` | `{ info, messages: [ChatMessage], active_turn? }` |
| `turn/start` | `{ session_id, text }` | `{ turn_id }`, returned at once; the turn runs in the background |
| `turn/cancel` | `{ session_id }` | `null` (no-op if nothing is running) |
| `ask/respond` | `{ ask_id, answer }` | `null`; error if no question with that id is open |
| `health/check` | `{}` | `[{ name, status: "ok" \| "warn" \| "error", message }]`: the core's checks (provider, API key, reachability, sessions folder, Lua) and core Lua's `bone.health` checks |
| `core/reload` | `{}` | `ReloadResult`. Loads the Lua configuration again (runtime, enabled plugins, `core.lua`) and switches to it; running turns finish on the previous one. An error (and no change) if loading fails |
| `plugin/list` | `{}` | `[{ name, core, loaded }]`: the plugins folder; `core` if it has a `core.lua`, `loaded` if that runs |
| `mcp/list` | `{}` | `[{ name, state, error?, tools }]`: the MCP servers core Lua configured; `state` is `"idle"`, `"starting"`, `"ready"` or `"failed"`, `tools` their tools by the names the model sees |
| `model/list` | `{}` | `[{ name, model, type?, current }]`: the `bone.config.providers` entries; `current` is the one turns use |
| `model/complete` | `{ provider?, messages, tools?, options?, stream? }` | `{ request_id }`, returned at once. One model call outside any session: `provider` is an entry name (default: the current one), `tools` are offered but never run, `options` override `model`, `reasoning_effort` or a Lua provider's options. With `stream`, `model/delta` events follow; `model/completed` always ends it |
| `model/cancel` | `{ request_id }` | `null`; the call ends with `model/completed` and the error `"cancelled"` |
| `plugin/load`, `plugin/unload`, `plugin/reload` | `{ name }` | `ReloadResult`: enable, disable, or keep the plugin, then reload as `core/reload` does. An error for a plugin that is not there or has no `core.lua` |

`ReloadResult` is `{ plugins: [{ name, core, loaded }], warnings? }`; `warnings` lists settings that cannot change while running (`data_dir`) and errors from `bone.on_shutdown`. Disabling a plugin lasts until the server restarts; rename its folder to disable it for good.

`SessionInfo` is `{ session_id, cwd, created_at, title? }`. `ChatMessage` is tagged by `role`:

```json
{ "role": "system", "content": "..." }
{ "role": "user", "content": "..." }
{ "role": "assistant", "content": "...", "reasoning": "...", "tool_calls": [{ "id", "name", "arguments" }] }
{ "role": "tool", "call_id": "...", "content": "...", "is_error": true }
```

Empty `content`/`reasoning`/`tool_calls` and a false `is_error` are omitted. A tool call's `arguments` is the raw JSON string the model produced, which may be invalid.

## Events

Every event carries `session_id` (except `echoed`, `ask/resolved`, `core/reloaded` and the `model/*` events) and, for turns, `turn_id`.

| Event | Params | Meaning |
|---|---|---|
| `turn/started` | `{ text }` | a user message started a turn (from any client) |
| `message/delta` | `{ kind: "text" \| "reasoning", text }` | streamed model output |
| `message/completed` | `{ message, usage? }` | the final assistant message as saved; replaces whatever streamed |
| `tool/started` | `{ call }` | the core is handling a tool call (`tool_call` hooks run next) |
| `tool/finished` | `{ call_id, output, is_error }` | its result, as the model will see it |
| `ask/requested` | `{ ask_id, question }` | core Lua (a hook or tool) called `bone.ask(question)` and waits; answer with `ask/respond`. `question` is whatever the Lua passed, e.g. the approve plugin's `{ kind: "approval", title, tool, arguments }` |
| `ask/resolved` | `{ ask_id, answer }` | answered by some client, or `answer: null` if the turn was cancelled first |
| `turn/finished` | `{ outcome: { status: "completed" \| "cancelled" \| "failed", message? } }` | the turn is over |
| `core/reloaded` | `ReloadResult` | the core switched to a newly loaded Lua configuration (no `session_id`) |
| `model/delta` | `{ request_id, kind, text }` | streamed output of a `model/complete` call (no `session_id`) |
| `model/completed` | `{ request_id, message?, usage?, error? }` | a `model/complete` call ended: the assistant `message`, or an `error` (no `session_id`) |
| `session/updated` | `{ reason: "append" \| "compact" }` | core Lua changed the session's transcript (`bone.session`); load it again with `session/messages` |

A typical turn: `turn/started`, then `message/delta`…, `message/completed` (with `tool_calls`), and for each call `tool/started`, then `ask/requested` → `ask/resolved` if a hook asks the user (the approve plugin does for tools that change things), then `tool/finished`. That repeats until a `message/completed` arrives without tool calls, then `turn/finished`.

Events are broadcast to all clients. A client that falls more than 8192 events behind loses the oldest ones, so treat `message/completed` and `session/messages` as authoritative over accumulated deltas.

## Errors

| Code | Meaning |
|---|---|
| `-32700` | the line was not valid JSON-RPC (sent with `"id": null`) |
| `-32601` | unknown method |
| `-32602` | bad params (including unknown session or ask ids) |
| `-32603` | internal error (e.g. session storage failed) |
| `-32002` | request before `initialize` |
| `-32003` | protocol version mismatch |
| `-32004` | the session already has a turn running |

## Rust client

```rust
let (client, mut events) = bone_client::Client::new(bone_client::connect_unix(path).await?);
client.initialize("my-tool").await?;
let s = client.request::<SessionCreate>(SessionCreateParams { cwd: Some(cwd) }).await?;
client.request::<TurnStart>(TurnStartParams { session_id: s.session_id, text: "hi".into() }).await?;
while let Some(e) = events.recv().await {
    if let Some(Ok(d)) = e.parse::<MessageDelta>() { print!("{}", d.text) }
    if e.parse::<TurnFinished>().is_some() { break }
}
```

`bone_client::spawn(Command::new("bone").arg("--headless"))` runs a private server as a child process. `crates/bone-server/examples/chat.rs` is a complete line-mode client.
