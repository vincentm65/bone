# Fork: Lua-owned conversation branches

A small paired plugin for bone. Requires a version with the generic core APIs
`bone.session.snapshot(id)` (`messages`, lossless `raw_messages`, summary,
model, active turn and generation) and seeded `bone.session.create(opts)`
accepting JSON-string message entries. It does not use
`session/fork`, edit session files, start a model turn, or modify Rust.

## Install

From the bone repository:

```sh
mkdir -p ~/.bone/plugins/fork
cp plugins/fork/{core.lua,tui.lua,README.md} ~/.bone/plugins/fork/
```

Restart bone, or run `/plugins reload fork` (load it with `/plugins load fork`
if it was not loaded already). Install both halves on the machine running
the core and the machine running the TUI if those are separate.

## Use

- `/fork`: centered, rounded family browser. Session rows show their children
  and selectable user-message points. Children with a known cutoff appear just
  before that user point; their edge is labeled **before #N**. Up/down, wheel,
  PageUp/PageDown and Home/End navigate; right expands, left collapses/goes to the parent;
  Escape or `q` closes. Enter on a session opens it. Enter on a user point
  creates a branch **before** that message, opens it, and restores that
  message's text and image references into the draft. Nothing is sent.
- `/fork N`: the same operation before the current session's **Nth user
  message**, counting from 1. Steering user messages count too. Invalid,
  fractional, nonpositive and out-of-range ordinals are rejected.
- `/fork copy`: copies the whole current transcript, with no selected-message
  prefill. The source must be idle.

A branch is an ordinary independent session, not a sub-agent. It keeps the
source working directory, pinned model and raw prefix (including reasoning,
sealed reasoning state, tool calls/results and image references). A compaction
summary comes along only when `summary.through <= copied message count`.
The selected user message is not in the copied history: edit its restored
prompt and explicitly submit when ready. The source is never changed.

Unresolved tool calls and orphan tool results in a copied prefix are rejected;
a steering cutoff may therefore be unsafe even though it is before a user
message. Safe earlier prefixes of an active source can be copied. All popup
selections are checked against a freshly read generation/content fingerprint;
if the source changed, reopen `/fork`. Snapshots are fetched lazily on expansion,
not during rendering. Image-bearing selections are refused on old TUIs lacking
`bone.prompt.set_images`; attachment errors are reported, not silently reduced
to a text-only prompt.

## Metadata, migration and backups

Genealogy is stored immediately in `~/.bone/state/core/fork.json`, via core-only
`bone.state.load/save("fork")`: child ID → parent ID, optional `before_turn`,
and prefix fingerprint. It is not written into new session headers. The core
RPC `fork.tree` returns family metadata plus one selected snapshot, its user
points and an `expected` token; `fork.create` accepts `session_id`, optional
`before_turn`, and optional `expected`. The TUI fetches `session/list` once per
popup and supplies that metadata for legacy import.

Old session headers' `parent` links are imported when seen in a snapshot or the
browser's session list. Their original cutoff is marked
**legacy (unknown cutoff)**; no old transcript is rewritten. Missing/deleted ancestor IDs remain visible
as non-openable placeholders. Unrelated sessions are not shown.

Back up the session store **and attachment blobs**, together with the core
`fork.json` state file. Losing plugin state loses new genealogy, not the
ordinary sessions. Uninstalling this plugin likewise leaves branches usable.
Images are blob references, not duplicate image files. Restoring just a session
file may leave references unresolved.

Creation and genealogy persistence are two separate operations: a crash/save
failure can leave a usable child without an edge (save failures are reported).
Closing/unloading while a create RPC is in flight prevents late navigation or
prompt edits, but cannot undo a child already created by the core. A snapshot
check and seeded creation are not an atomic transaction: concurrent changes
or starting a new source turn after the check cannot be locked out by these
generic APIs; the copied snapshot itself remains consistent and validated.
Fingerprints are optimistic noncryptographic checks. Avoid concurrent cores
sharing the same state file (last writer wins). No search/rename/delete UI or
automatic draft persistence is included.

Lossless copying uses `snapshot.raw_messages`: an array of exact JSON strings
passed directly as the seeded `session.create` message entries. Nested provider
arguments and sealed reasoning retain JSON `null` and empty arrays without a
JSON↔Lua round trip. Parsed `snapshot.messages` is used only for user points and
tool-prefix safety checks; stale-view fingerprints use the raw strings when
available. Older snapshots without `raw_messages` fall back to parsed entries.

## Fixture tests

```sh
lua plugins/fork/test.lua
luajit plugins/fork/test.lua
luac -p plugins/fork/core.lua plugins/fork/tui.lua plugins/fork/test.lua
```

Standalone Lua/LuaJIT tests use fake bone APIs: exact JSON-string and parsed
message/model/image/summary copying, ordinal validation, stale selections,
active-source and dangling-tool safety, family/legacy metadata, fork-point
ordering, missing/cyclic ancestors, delayed prefill, closed/unloaded callbacks,
and no render-time RPCs. They do not substitute for a live terminal integration test.
