# Automatic Lazy Lua Reload Plan

## Goal

Automatically apply Lua source edits at the next safe interaction boundary without a filesystem watcher or background polling. Preserve `/tools reload` as an explicit escape hatch.

## 1. Add deterministic extension-source fingerprints

Create a small module such as `core/src/ext/source_stamp.rs`.

Fingerprint:

- `init.lua`
- every `*.lua` file recursively under `lua/`
  - tools
  - commands
  - libraries
  - plugins
  - themes
- sorted relative paths plus file contents
- include paths so adding, deleting, or renaming files changes the result

Use a content hash rather than modification timestamps. The extension tree is small, and content hashing avoids missing same-size edits or coarse filesystem timestamps.

Reading should produce one of:

```rust
enum SourceStamp {
    Ready(Hash),
    Unreadable(/* stable error identity */),
}
```

An incomplete or unreadable scan must not be recorded as successfully loaded.

## 2. Track loaded and attempted versions

Maintain:

```rust
struct ExtensionSourceState {
    loaded: SourceHash,
    attempted: Option<SourceHash>,
}
```

Initialize `loaded` from the source tree associated with the initial successful boot. Do not treat an unreadable scan as loaded or attempted.

Decision rules:

- `current == loaded`: do nothing
- `current == attempted`: do nothing; this exact version was already tried
- otherwise atomically claim the version by setting `attempted = current`
- only the actor that wins the claim boots a candidate VM

Successful claimant sequence:

1. boot and validate a candidate without replacing the working VM
2. recompute the fingerprint afterward
3. if the fingerprint is still `current`:
   - replace the working VM
   - set `loaded = current`
   - set `attempted = None`
   - then notify peer actors
4. if files changed during boot, the candidate may be installed, but do not mark the newer fingerprint loaded; the next interaction attempts it

Failure sequence:

1. retain the previous VM
2. retain `attempted = current` so the same broken edit is not retried
3. do not notify peer actors

This prevents reloads on every turn, prevents every actor from retrying the same broken edit, and ensures peers are notified only about a validated source version.

## 3. Detect changes at the existing safe interaction boundary

Check immediately before handling:

- `RuntimeCommand::SubmitPrompt`
- `RuntimeCommand::RunCommand`
- daemon-generated background prompt injection, which already routes through `SubmitPrompt`

The central location is `DaemonCtx::handle_idle_command()` around `core/src/rpc/mod.rs:1933`, before the command mutates the transcript or looks up a Lua command.

This gives predictable behavior:

- edits never replace a VM during a turn or tool call
- a changed command is available before command lookup
- hooks and tools are refreshed before the next prompt is recorded
- multiple saves before the next interaction collapse into one reload

Do not use the existing 200 ms background timer for detection; that would effectively be polling.

## 4. Share detection state across daemon actors

Put the source state in `HubGroup`, not independently in each cached conversation actor. Protect it with a mutex and expose an atomic `try_claim(fingerprint)` operation; hashing happens outside the lock, while comparison and claim happen inside it.

The first actor that successfully claims a new fingerprint:

1. boots and validates its own candidate VM as catalog authority
2. installs the candidate only on success
3. commits `loaded` and clears `attempted` before notification when the post-boot fingerprint is unchanged
4. only after success, uses `HubGroup::request_extension_reload()` with itself as `skip_conversation_id`
5. causes other cached actors to reload as non-authorities when they become idle

A failed claimant retains its previous VM and does not broadcast. Other actors see `current == attempted` and do not retry the same broken version. Busy peers already defer successful group reloads until their current work completes.

Ungrouped runtimes own the same tracker locally in `DaemonCtx`.

## 5. Make automatic reload atomic on Lua errors

There is one important prerequisite: the current boot path can return a VM that exists but did not load all Lua successfully:

- `init.lua` execution errors are logged inside `core/src/ext/engine.rs`; `run_init()` returns `Ok(false)`, so the manager has `engine_ok = true` but `loaded = false` and hooks become no-ops
- tool/command file execution errors are logged and skipped in `core/src/ext/mod.rs`; the enclosing loader still returns success
- `DaemonCtx::reload_extensions()` currently guards unavailable managers only on the disk-boot path, while the in-process handoff path assigns its candidate unconditionally

Extend boot results with structured diagnostics, for example:

```rust
struct BootResult {
    // existing fields...
    source_errors: Vec<ExtensionLoadError>,
}
```

Then distinguish reload reason:

```rust
enum ReloadReason {
    Manual,
    Automatic,
}
```

Recommended semantics:

- **All reload reasons:** build a candidate first and run one acceptance check before assigning `self.extensions`; engine failure, `init.lua` failure, or any tool/command source error rejects the candidate on both disk and in-process handoff paths.
- **Automatic reload:** retain the previous VM, retain the attempted fingerprint, and do not broadcast on rejection.
- **Manual `/tools reload`:** use the same atomic acceptance behavior, but always rescan regardless of `loaded` or `attempted`.

Report one concise status message, while detailed diagnostics continue going to the Lua log.

## 6. Keep manual reload authoritative

`/tools reload` remains available and should:

- always attempt a reload, regardless of the fingerprint
- update `loaded` after success
- clear `attempted`
- preserve the existing conversation-scoped state handoff in `ToolHandler::adopt_session_state_from()`

Catalog-triggered reloads should also refresh the stored fingerprint so the next interaction does not reload the same files again.

## 7. Scope the first version to Lua

Automatically track Lua extension sources only.

Do **not** include:

- `config.yaml`
- `providers.yaml`
- `subagents.yaml`
- `extensions.yaml`
- `command-policy.yaml`

Those files have different ownership and application semantics. Configuration mutations already have revisioned APIs, and command policy is explicitly restart-only.

## 8. Tests

### Fingerprint unit tests

Add focused tests near the new module:

- unchanged tree yields the same fingerprint
- changed contents yield a different fingerprint
- same-size content changes are detected
- add, delete, and rename are detected
- nested library/plugin files are included
- non-Lua files are ignored
- traversal and unreadable-file errors are handled deterministically

### Reload-state tests

Verify:

- one changed fingerprint causes one attempt
- unchanged fingerprint causes no attempt
- the same failed fingerprint is not retried
- editing the broken source creates a new attempt
- changing files during reload leaves another reload pending
- successful manual/catalog reload updates the stored fingerprint

### Runtime integration tests

Extend `core/tests/lua_api_test.rs` and `core/src/rpc/rpc_tests.rs`:

1. Start with tool implementation A.
2. Execute or expose A.
3. Edit it to implementation B.
4. Submit the next interaction without `/tools reload`.
5. Verify B is active.
6. Submit again and verify no second reload status/event.
7. Introduce invalid Lua and verify the last working runtime remains active.
8. Repeat without editing and verify no retry.
9. Fix the file and verify it reloads once.
10. Verify `ctx.state`, tool host state, snapshots, and conversation history survive.
11. Verify one actor detects a change and grouped cached actors each reload once at idle.

## 9. User-facing documentation

Update `README.md` and `core/defaults/docs/extension-api.md`:

> Lua source edits are detected lazily and applied before the next prompt or Lua command. Bone does not watch or poll the filesystem. `/tools reload` remains available for an explicit rescan.

## 10. Validation

Run:

- formatting
- focused extension and RPC tests
- full core test suite
- relevant TUI tests
- a real `tmux` PTY smoke test with isolated `BONE_DIR`:
  1. launch Bone
  2. run a Lua command/tool
  3. edit its source externally
  4. run it again without `/tools reload`
  5. verify the new behavior
  6. verify invalid Lua preserves the working version
  7. fix it and verify the next interaction reloads
  8. exit cleanly

## Estimate

Small-to-medium, likely 1–2 focused days. Fingerprinting and triggering are straightforward; making reload atomic on Lua syntax/runtime errors is the main correctness work.
