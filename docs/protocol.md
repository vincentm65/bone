# The bone protocol

Everything that talks to the bone core (the TUI, scripts, editors) uses this
API. It is JSON-RPC 2.0, one JSON message per line (newline-delimited JSON).

The typed definitions in `crates/bone-proto/src/methods.rs` and `types.rs` are the source of truth. Exact wire examples for every method and event are in `crates/bone-proto/tests/golden/`; tests fail if the wire format changes.

## Connecting

| Transport | How |
|---|---|
| stdio | start `bone --headless`, write requests to its stdin, read its stdout |
| Windows named pipe | `bone --headless --listen [PATH]`, default `\\.\pipe\bone3-<USERNAME>`; restricted to the current user and local clients |
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

After `initialize`, the server runs each request on its own task: replies carry their request's `id` and can arrive in any order, and a request's events (for example `model/delta`) can arrive before its reply.

| Method | Params | Result |
|---|---|---|
| `initialize` | `{ protocol_version, client_name }` | `{ protocol_version, server_name, server_version }` |
| `shutdown` | `{}` | `null`; then the server closes this connection |
| `echo` | `{ text }` | `{ text }`, plus an `echoed` event (for testing) |
| `session/create` | `{ cwd? }` | `SessionInfo`. `cwd` defaults to the server's directory, so clients should send their own |
| `session/list` | `{}` | `[SessionInfo]`, newest first |
| `session/active` | `{}` | `[SessionId]` for currently running turns in the core (ordering unspecified); no transcripts are loaded |
| `session/messages` | `{ session_id }` | `{ info, messages: [ChatMessage], active_turn? }` |
| `session/rename` | `{ session_id, title }` | `SessionInfo` with the new title (kept in `<id>.title` next to the session file); a `session/updated` event with `reason: "rename"` follows |
| `session/fork` | `{ session_id, before_turn? }` | `SessionInfo` of a new session holding a copy of the transcript (with `before_turn = N`, only what came before the Nth user message), its `parent` set. The original is untouched |
| `session/delete` | `{ session_id }` | `null`; the session and its file are gone and `session/deleted` goes to every client. An error while a turn runs |
| `session/compact` | `{ session_id, clear? }` | `{ session_id, messages, tokens_before, tokens_after, reason }`: the older part of the session is summarized by a model, and from then on model calls get the summary in place of it. The transcript, the session file and `session/messages` keep everything. `messages` is how many transcript messages were newly summarized; the token counts are estimates of what a model call sends, before and after. `clear` drops the summary instead (`reason: "clear"`). An error when there is nothing to compact yet. Also announced as `session/compacted` |
| `attachment/upload` | `{ data, name? }` (base64 PNG/JPEG/WebP) | `ImageAttachment`: validates, normalizes and stores the image on the core machine |
| `attachment/read` | `{ id }` | `{ data }`: base64 PNG for a saved attachment |
| `turn/start` | `{ session_id, text, images? }` | `{ turn_id }`, returned at once; the turn runs in the background |
| `turn/steer` | `{ session_id, text, images? }` | `null`; shorthand for `queue/add` with `mode: "steer"`, but an error when no turn is running |
| `queue/add` | `{ session_id, text, mode?, images? }` | `{ id? , turn_id? }`. Idle session: the message starts a turn at once (`turn_id`). Running turn: it is queued (`id`); `mode: "steer"` (the default) joins that turn before its next model call (`turn/steered` says when; one arriving during the final answer gets another step), `"next"` starts its own turn after it. Queued turns go through `turn_start` hooks; steered messages do not. Core Lua `queue_add` hooks may rewrite or refuse it |
| `queue/remove` | `{ session_id, id }` | `null`; an error for an unknown id |
| `queue/update` | `{ session_id, id, text?, mode?, images? }` | `null` |
| `queue/move` | `{ session_id, id, to }` | `null`; `to` is the new position, 0 first |
| `queue/clear` | `{ session_id }` | `null` |
| `queue/resume` | `{ session_id }` | `null`; a paused queue goes on (starting a turn if the session is idle) |
| `turn/cancel` | `{ session_id }` | `null` (no-op if nothing is running) |
| `processes/get` | `{ session_id }` | `{ version, processes: [ProcessSnapshot] }` for managed shell processes |
| `processes/list` | `{}` | the same, for every session's processes |
| `process/cancel` | `{ session_id, id }` | `null`; asks the process group to stop |
| `process/read` | `{ session_id, id, from? }` | `{ offset, data, total }`: the output as written (escape codes kept) from byte `from` (default 0); `offset` is later than asked when older output is no longer kept (the last 1 MiB is), `total` is how many bytes it wrote |
| `process/resize` | `{ session_id, id, cols, rows }` | `null`; sets a running terminal job's size (it gets `SIGWINCH`) |
| `ask/respond` | `{ ask_id, answer }` | `null`; error if no question with that id is open |
| `health/check` | `{}` | `[{ name, status: "ok" \| "warn" \| "error", message }]`: the core's checks (provider, API key, reachability, sessions folder, Lua) and core Lua's `bone.health` checks |
| `settings/get` | `{}` | the saved settings (`settings.json` in the config dir), a JSON object |
| `settings/set` | `{ path, value }` | every setting after the change. `path` is dotted (`tui.tool_detail`, `providers.qwen.model`); `null` removes it. `provider` must name a configured provider; `providers.<name>` adds a provider (`{ base_url, model, type, api_key_env }`) and `providers.<name>.<field>` changes one field of any provider; changing either reloads the configuration, and the change is undone if that fails. Every client hears `settings/changed` |
| `secrets/set` | `{ provider, key }` | the names of the providers with a key. Saves (or with `key: null` removes) a provider's API key in `secrets.json` (mode 0600) and reloads. Keys are never sent to clients |
| `secrets/list` | `{}` | the names of the providers with a saved key |
| `settings/reset` | `{ path }` | as `settings/set` with `null` |
| `core/reload` | `{}` | `ReloadResult`. Loads the Lua configuration again (runtime, enabled plugins, `core.lua`) and switches to it; running turns finish on the previous one. An error (and no change) if loading fails |
| `plugin/list` | `{}` | `[{ name, core, loaded }]`: the plugins folder; `core` if it has a `core.lua`, `loaded` if that runs |
| `lua/call` | `{ name, args?, session_id?, cwd? }` | whatever the function core Lua registered as `name` (`bone.rpc.register`) returns; it gets `args` and `{ session_id, cwd }`. An error for an unknown name or a failing function |
| `mcp/list` | `{}` | `[{ name, state, error?, tools }]`: the MCP servers core Lua configured; `state` is `"idle"`, `"starting"`, `"ready"` or `"failed"`, `tools` their tools by the names the model sees |
| `store/query` | `{ sql, params? }` | `{ columns, rows, truncated }`: one read-only SQL statement against the session index (see architecture.md for its tables). `params` is a list (`?1`…) or an object (`:name`); at most 10,000 rows, 5 seconds |
| `model/list` | `{}` | `[{ name, model, type?, current, supports_images? }]`: the `bone.config.providers` entries; `current` is the one turns use |
| `model/complete` | `{ provider?, messages, tools?, options?, stream? }` | `{ request_id }`, returned at once. One model call outside any session: `provider` is an entry name (default: the current one), `tools` are offered but never run, `options` override `model`, `reasoning_effort` or a Lua provider's options. With `stream`, `model/delta` events follow; `model/completed` always ends it |
| `model/cancel` | `{ request_id }` | `null`; the call ends with `model/completed` and the error `"cancelled"` |
| `plugin/load`, `plugin/unload`, `plugin/reload` | `{ name }` | `ReloadResult`: enable, disable, or keep the plugin, then reload as `core/reload` does. An error for a plugin that is not there or has no `core.lua` |

`ImageAttachment` is `{ id, name, mime_type, width, height, bytes }`, where `id` is the SHA-256 of a canonical PNG, `mime_type` is `"image/png"`, and `bytes` is its stored size. First upload bytes, then put returned references in `images` on user messages, turn or queue requests, or `model/complete` messages. Local client paths are never sent to the core. A message may have empty text when it has images. `queue/update` omits `images` to keep them or sends `images: []` to remove them; an empty message is rejected. Empty lists are omitted on the wire, preserving existing text-only messages.

Limits are 8 images/message, 20 MiB/image input and canonical PNG, 40 megapixels/image, 40 MiB/message, and 32 MiB per NDJSON frame. Image bytes are rejected inside references: only providers receive an extra `data` field with hydrated base64 PNG. Caller-supplied metadata is replaced by verified stored metadata. Sessions and queue files save references, so attachment files must move with the data directory. Uploads deduplicate by hash and are retained indefinitely.

`ReloadResult` is `{ plugins: [{ name, core, loaded }], warnings? }`; `warnings` lists settings that cannot change while running (`data_dir`) and errors from `bone.on_shutdown`. Disabling a plugin lasts until the server restarts; rename its folder to disable it for good.

`SessionInfo` is `{ session_id, cwd, created_at, title?, parent?, owner? }`. `parent` is the session it was forked from; `owner` is `{ session_id, call_id?, name? }` for a session core Lua started on another's behalf (a sub-agent, via `bone.session.create`): the session and tool call that started it. Clients usually leave owned sessions out of session pickers and show them with their owner.

The queue: each session keeps `[{ id, text, images?, mode: "steer" | "next", created_at }]`, saved next to its file (`<id>.queue.json`) so it outlives a restart. When a turn ends, any steer message that did not join it becomes `next`, and the first queued message starts the next turn, after completed and failed turns alike. A cancelled turn pauses the queue, and so does loading a session that had a queue; `queue/resume` or a new `queue/add` lets it go on. `session/messages` includes `queue` and `queue_paused`. `ChatMessage` is tagged by `role`:

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
| `turn/started` | `{ text, images? }` | a user message started a turn (from any client) |
| `message/delta` | `{ kind: "text" \| "reasoning", text }` | streamed model output |
| `message/completed` | `{ message, usage? }` | the final assistant message as saved; replaces whatever streamed |
| `tool/started` | `{ call, started_at? }` | the core is handling a tool call (`tool_call` hooks run next); `started_at` is milliseconds since the Unix epoch |
| `tool/output` | `{ call_id, text }` | output a running foreground tool produced, in order, as it comes; `tool/finished` still carries the whole result. Detached shell processes use `process/changed` after their start result |
| `tool/finished` | `{ call_id, output, is_error, duration_ms? }` | its result, as the model will see it, and how long it ran |
| `process/changed` | `{ session_id, version, process: ProcessSnapshot, chunk? }` | a managed shell process started, produced output, or changed state. `chunk` is `{ offset, data }`: new output as written, at byte `offset` (chunks follow on from each other; after a gap, `process/read` it) |
| `ask/requested` | `{ ask_id, question }` | core Lua (a hook or tool) called `bone.ask(question)` and waits; answer with `ask/respond`. `question` is whatever the Lua passed, e.g. `{ kind: "confirm", text: "Write this file?" }` |
| `ask/resolved` | `{ ask_id, answer }` | answered by some client, or `answer: null` if the turn was cancelled first |
| `turn/steered` | `{ text }` | a steer message from the queue joined the running turn's transcript (show it as a user message) |
| `queue/changed` | `{ items, paused, error? }` | the session's queue after a change; a failed start retains the item, pauses the queue and includes its error |
| `turn/finished` | `{ outcome: { status: "completed" \| "cancelled" \| "failed", message? } }` | the turn is over |
| `settings/changed` | `{ path, value, settings }` | a setting was saved (`value` null: removed) by some client; `settings` is all of them (no `session_id`) |
| `core/reloaded` | `ReloadResult` | the core switched to a newly loaded Lua configuration (no `session_id`) |
| `model/delta` | `{ request_id, kind, text }` | streamed output of a `model/complete` call (no `session_id`) |
| `model/completed` | `{ request_id, message?, usage?, error? }` | a `model/complete` call ended: the assistant `message`, or an `error` (no `session_id`) |
| `session/created` | `SessionInfo` | a session was created: by a client, a fork, or core Lua (a sub-agent's, with `owner`) |
| `session/deleted` | `{ session_id }` | a client deleted the session |
| `session/compacted` | `{ messages, tokens_before, tokens_after, reason }` | the session was compacted (`reason`: `"manual"` from `session/compact`, `"limit"` over `compact.limit`, `"overflow"` after the model said the context is too long, or `"clear"`). The transcript did not change; there is nothing to reload |
| `session/compact_failed` | `{ reason, error }` | automatic compaction failed (`reason`: `"limit"` or `"overflow"`); the context is unchanged. Future calls can attempt compaction again. Manual failures are returned by `session/compact` instead |
| `session/updated` | `{ reason: "append" \| "compact" \| "rename" \| "lagged" }` | core Lua changed the session's transcript (`bone.session.append`, or `bone.session.compact` replacing it), or it was renamed; load it again with `session/messages`. `"lagged"`, with `session_id` empty: this client fell behind and missed events, so load every session it shows again |

A typical turn: `turn/started`, then `message/delta`…, `message/completed` (with `tool_calls`), and for each call `tool/started`, then `ask/requested` → `ask/resolved` if a hook asks the user (for example, a custom approval hook), then `tool/finished`. That repeats until a `message/completed` arrives without tool calls, then `turn/finished`.

Events are broadcast to all clients. A client that falls more than 8192 events behind loses the oldest ones, so treat `message/completed` and `session/messages` as authoritative over accumulated deltas.

`ProcessSnapshot` contains `id`, `command`, `state` (`running`, `exited`,
`cancelled`, `timed_out` or `failed`), `running`, `pid`, start/finish times,
`tail` (the last line of output, escape codes removed), `output_bytes`,
`truncated`, exit status, and `terminal`. Its output comes as `process/changed`
chunks and from `process/read`.
Background shell processes are owned by the session and outlive the model turn;
`process/changed` is the authoritative UI stream after the shell tool returns.
They run in a pseudo-terminal (`terminal: true`, 120×32 until `process/resize`;
`TERM=xterm-256color`, `PAGER=cat`), so their output is what a terminal shows:
colors and `\r` progress lines. The model reads it with escape codes removed and
progress lines settled.

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
let (client, mut events) = bone_client::Client::new(bone_client::connect_local(path).await?);
client.initialize("my-tool").await?;
let s = client.request::<SessionCreate>(SessionCreateParams { cwd: Some(cwd) }).await?;
client.request::<TurnStart>(TurnStartParams { session_id: s.session_id, text: "hi".into() }).await?;
while let Some(e) = events.recv().await {
    if let Some(Ok(d)) = e.parse::<MessageDelta>() { print!("{}", d.text) }
    if e.parse::<TurnFinished>().is_some() { break }
}
```

`bone_client::spawn(Command::new("bone").arg("--headless"))` runs a private server as a child process. `crates/bone-server/examples/chat.rs` is a complete line-mode client.
