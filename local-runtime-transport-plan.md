# Strict plan: local TUI runtime transport

Goal:

```text
bone
  one process
  Rust TUI client only
  in-process transport
  runtime host owns Lua/tools/commands/session/model

bone serve
  explicit socket server for external clients
```

No code changes until approved.

## Non-goals

Do not:

- move Lua/tool/command execution into the TUI
- resurrect direct TUI/runtime coupling
- remove `bone serve`
- remove `bone --connect`
- add global daemon discovery
- add singleton server behavior
- add session multiplexing
- change protocol types unless unavoidable
- change user-facing command semantics except default `bone` no longer spawning child `serve`

## Desired architecture

Default local TUI:

```text
bone process
  Rust TUI client
    ⇄ RuntimeCommand / RuntimeEvent via in-process Hub
  runtime host
    owns RuntimeSession
    owns Lua VM/extensions
    owns tools/commands
    owns provider calls
    owns DB writes
```

Explicit server:

```text
bone serve
  runtime host
    ⇄ socket RPC clients
```

## Step 1: Extract reusable runtime-host boot

File: `tui/src/main.rs`

Current `run_serve()` contains runtime boot logic:

- parse provider/model
- load custom config
- create provider
- boot Lua/tools via `boot_with_tools`
- create `RuntimeSession`
- init DB
- create `Hub`
- call `run_daemon`

Extract the shared boot pieces into a small helper, probably local to `main.rs` first:

```rust
struct RuntimeHostBoot {
    provider: Arc<dyn LlmProvider>,
    provider_id: String,
    booted: bone::ext::BootedTools,
    session: Arc<Mutex<RuntimeSession>>,
}
```

Function shape:

```rust
fn boot_runtime_host(
    provider_id: String,
    model_override: Option<String>,
    custom: CustomConfigs,
    headless: bool,
) -> std::io::Result<RuntimeHostBoot>
```

Keep it minimal. Do not move to a new module unless needed.

## Step 2: Add local runtime client constructor

File: `tui/src/ui/app/mod.rs`

Current `App::with_daemon(...)` accepts `RemoteClient`.

Add a lower-level constructor that accepts the already-normalized client pieces:

```rust
pub fn with_runtime_client(
    provider: Box<dyn LlmProvider> or Arc/Box existing type,
    cfg: UserConfig,
    custom: CustomConfigs,
    command_tx: UnboundedSender<RuntimeCommand>,
    events_rx: broadcast::Receiver<RuntimeEvent>,
    daemon_client: Option<RemoteClient>,
) -> std::io::Result<Self>
```

Then make existing `with_daemon(...)` just call this:

```rust
let command_tx = client.command_sender();
let events_rx = client.subscribe();
Self::with_runtime_client(..., command_tx, events_rx, Some(client))
```

Important:

- TUI still gets only `command_tx` and `events_rx`.
- TUI still does not own Lua/tools.
- `_daemon_client` remains only to keep remote socket bridge alive.

## Step 3: Add default in-process path

File: `tui/src/main.rs`

Replace default path:

```rust
spawn_local_daemon(...)
connect_with_retry(...)
RemoteClient::connect(...)
App::with_daemon(...)
```

with:

```rust
let (hub, commands_rx) = bone::rpc::Hub::new();
let command_tx = hub.command_sender();
let events_rx = hub.subscribe();

boot runtime host in this process

start run_daemon in-process

App::with_runtime_client(..., command_tx, events_rx, None)
```

## Step 4: Handle `!Send` correctly

This is the key risk.

`run_daemon()` cannot be spawned with normal `tokio::spawn` if the future is `!Send`.

Use one of these, in order:

### Preferred: `tokio::task::LocalSet`

Change default `main` flow so local runtime host runs on a local task:

```rust
let local = tokio::task::LocalSet::new();

local
  .run_until(async move {
      tokio::task::spawn_local(bone::rpc::run_daemon(...));
      app.run().await
  })
  .await
```

Only use this for default local TUI path.

Do not change `bone serve` unless needed.

### If `spawn_local` still fails

Use a `select!` where the daemon future and TUI future are both driven on the same task/local set.

Do not fall back to child process.

## Step 5: Preserve `bone serve`

Keep `run_serve()` behavior intact:

```sh
bone serve --listen 127.0.0.1:7878
```

It should still:

- bind TCP
- accept many clients
- send `FrontendState`
- send `StateSnapshot`
- own runtime/session/Lua/tools
- use `serve_connection`

Only deduplicate boot logic if clean.

## Step 6: Remove default child daemon path

After local path works, remove or stop using:

```rust
spawn_local_daemon()
connect_with_retry()
ChildGuard
```

If only used by old default path, delete them.

Keep manual `bone serve`.

## Step 7: Update comments/help text

Update misleading comments:

- no longer say default is “Phase 3-pure spawn local serve”
- docs/comments should say:
  - default `bone` uses in-process runtime host over channels
  - `bone serve` exposes same runtime protocol over socket

Usage text can stay mostly the same, but remove implication that `bone` secretly uses `serve`.

## Step 8: Tests

Run existing:

```sh
cargo build
cargo test
```

Add targeted tests if practical:

1. `App::with_daemon` still wraps `RemoteClient`.
2. local runtime client constructor stores command/event handles correctly.
3. `bone serve` tests still pass.
4. no default startup helpers for child daemon remain.

Do not add broad flaky TUI integration tests unless existing patterns support it.

## Step 9: Manual verification

After build/tests:

```sh
cargo run -p bone-tui -- --help
cargo run -p bone-tui -- serve --listen 127.0.0.1:7878
cargo run -p bone-tui -- --connect 127.0.0.1:7878
cargo run -p bone-tui --
```

Also verify normal `bone` no longer starts a child `bone serve` process.

## Acceptance criteria

The change is done only if:

- [x] `bone` starts one process for normal local TUI
- [x] Rust TUI remains client-only
- [x] Lua/tools/commands still owned by runtime host
- [x] `bone serve` still works
- [x] `bone --connect <addr>` still works
- [x] no TCP port is opened for default `bone`
- [x] no child `bone serve` is spawned for default `bone`
- [x] local and remote paths share `RuntimeCommand` / `RuntimeEvent`
- [x] no duplicated local-only runtime behavior is introduced
- [x] `cargo build` passes
- [x] `cargo test` passes
