# Plan: `/undo` — branching conversation history

Status: design, not implemented. Last updated against schema version 4.

## 1. Goal

A Lua-first `/undo` command that lets the user rewind a conversation to a prior
turn **and** restores the files the model changed, without touching the user's
main git repository. Rewind points are presented as a navigable **tree**.

Non-goals:
- Reversing non-file side effects (shell installs, network, processes). No tool
  does this; we won't pretend to.
- Refunding token cost (`usage_events` is immutable history).
- Sub-turn rewind (rewinding inside an assistant→tool-call→tool-result chain
  corrupts the provider transcript). Rewind snaps to **user-message boundaries**.

## 2. Core decision: branch, don't delete

Every deletion-based approach (truncate, generic `db.exec` DELETE) collides with
a single invariant: `messages` and `messages_fts` must stay in sync, and deleted
rows resurrect / re-order on reload. The branching model dissolves the problem
by **never deleting**.

| | delete-based | branch-based (chosen) |
|---|---|---|
| `/undo` | `DELETE ... seq > N` | move an `active_tip` pointer back |
| reload after undo | rewound rows resurrect | nothing to resurrect — rows still exist |
| FTS sync | a problem to solve via triggers/verbs | unchanged (no deletes) |
| "go back further later" | impossible (gone) | always available |
| fork / explore alternatives | impossible | the natural data model |
| the "tree" UI | rendered atop linear data | the literal data model |
| storage growth | bounded | grows with branching (GC later) |

The tree the user asked for is not a UI bolted onto linear history — it **is**
the data model. `/undo` is the first consumer; "fork a branch", "compare two
approaches", "bookmark a state" are future features that come for free.

### Why this also fixes the reload problem

There is no deletion to get out of sync. The DB stores the full tree + an active
tip; memory loads the active path. DB and memory agree **by construction**. This
removes the entire truncate/FTS debate that dominated earlier discussion.

## 3. Schema changes (migration to v5)

Current schema (`session_db.rs`, `FULL_SCHEMA`): `messages` has
`id, conversation_id, role, content, tool_name, tool_call_id, tool_calls, seq,
created_at`. `seq` is a monotonic per-conversation counter.

### 3a. `messages.parent_id` + `conversations.active_tip_id`

```sql
-- v4 -> v5
ALTER TABLE messages ADD COLUMN parent_id INTEGER REFERENCES messages(id);
ALTER TABLE conversations ADD COLUMN active_tip_id INTEGER REFERENCES messages(id);
CREATE INDEX idx_messages_parent ON messages(parent_id);
```

Semantics:
- `parent_id` = the message this one was appended after. A linear conversation is
  a degenerate tree where each node's parent is the previous node.
- `active_tip_id` = the current head of the active branch. The next turn appends
  with `parent_id = active_tip_id` and then advances `active_tip_id` to the new
  message.
- `seq` stays as a monotonic insertion counter (useful for ordering children of
  the same parent, debugging). It **no longer defines the active transcript** —
  see §3c.

### 3b. Backfill on migration

For every existing conversation, set each message's `parent_id` to the previous
message's `id` (by `seq`), and set `active_tip_id` to the max-`seq` message:

```sql
-- run inside the v4->v5 transaction; conceptually, per conversation:
UPDATE messages SET parent_id = (
  SELECT m2.id FROM messages m2
  WHERE m2.conversation_id = messages.conversation_id
    AND m2.seq < messages.seq
  ORDER BY m2.seq DESC LIMIT 1
) WHERE parent_id IS NULL;

UPDATE conversations SET active_tip_id = (
  SELECT id FROM messages
  WHERE messages.conversation_id = conversations.id
  ORDER BY seq DESC LIMIT 1
) WHERE active_tip_id IS NULL;
```

Root messages (lowest seq, first message) keep `parent_id NULL`.

### 3c. Read path changes

The active transcript is now **the path from `active_tip_id` back to the root via
`parent_id`**, not a `seq`-ordered scan. Affected code:
- `load_conversation` / history-load in `ui/app/mod.rs` (uses
  `max_message_seq` + seq scan today).
- Headless resume in `agent.rs`.

Replacement query (recursive CTE):

```sql
WITH RECURSIVE path(id) AS (
  SELECT active_tip_id FROM conversations WHERE id = ?1
  UNION ALL
  SELECT m.parent_id FROM messages m JOIN path p ON m.id = p.id
  WHERE m.parent_id IS NOT NULL
)
SELECT m.* FROM messages m JOIN path p ON m.id = p.id
ORDER BY m.seq ASC;
```

`max_message_seq` for the `session_seq` counter becomes
`SELECT MAX(seq) FROM messages WHERE conversation_id = ?1` (unchanged; seq is
still monotonic per conversation regardless of branching).

### 3d. Write path changes

`append_message` gains a `parent_id` param. The caller supplies the current
`active_tip_id` (or NULL for the first message), then advances the tip:

```sql
INSERT INTO messages (conversation_id, role, content, tool_name, tool_call_id,
                      tool_calls, seq, created_at, parent_id)
VALUES (...);
-- new row id is the new tip
UPDATE conversations SET active_tip_id = last_insert_rowid() WHERE id = ?;
```

## 4. Platform change: general `ctx.db.exec` + FTS triggers

Two independent changes that together let the `/undo` plugin be pure Lua.

### 4a. `ctx.db.exec(sql, params)` — general write primitive

Mirror of the existing `ctx.db.query` (`ctx.rs:1029`) minus the SELECT-only
guard. **DML only** (`INSERT`/`UPDATE`/`DELETE`); reject
`CREATE`/`DROP`/`ALTER`/`PRAGMA`/`ATTACH`/`DETACH`. This keeps the migration
system (`user_version`) safe while giving plugins write power over data.

Returns `{ rows_affected = n, last_insert_rowid = id }`.

Rationale: bone is meant to be the "neovim of agents". Bespoke verbs per feature
(`truncate`, `tag`, `export`…) fight that. One general write primitive, guarded
against schema damage, serves every future plugin.

### 4b. FTS invariant moves into the schema

`append_message` (`session_db.rs:428`) currently inserts into `messages_fts`
manually, with `TOOL_CALL`-augmented search text. Replace this with triggers so
**no caller** (Rust, Lua, or future feature) can desync FTS:

```sql
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts (rowid, content, role, conversation_id)
  VALUES (new.id,
          CASE WHEN new.tool_calls IS NOT NULL
               THEN new.content || ' TOOL_CALL ' || new.tool_calls
               ELSE new.content END,
          new.role, new.conversation_id);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = old.id;
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = old.id;
  INSERT INTO messages_fts (rowid, content, role, conversation_id)
  VALUES (new.id,
          CASE WHEN new.tool_calls IS NOT NULL
               THEN new.content || ' TOOL_CALL ' || new.tool_calls
               ELSE new.content END,
          new.role, new.conversation_id);
END;
```

Then remove the manual FTS `INSERT` from `append_message`. With triggers in
place, a Lua plugin can `DELETE`/`UPDATE` `messages` freely and search stays
correct. (Under the branch model we rarely delete, but the safety is free and
protects every consumer.)

The delete/update triggers are only needed if we ever prune branches (§7, GC).
The insert trigger is required immediately and replaces existing manual logic.

## 5. File restoration via shadow git (never the main repo)

A separate git object store indexes the user's real working tree. The user's
`.git` is untouched and unobservable.

- Repo path: `~/.bone-rust/undo/<conversation_id>.git` (bare, external).
- All commands scoped: `git --git-dir=<shadow> --work-tree=<cwd> ...`
- Snapshot = `git add -A && git commit` of the working set. `.gitignore` is
  respected (so e.g. `target/`, `node_modules/` are excluded).
- Restore = `git --git-dir=<shadow> checkout <commit> -- .` into the working tree.

This runs entirely through `ctx.shell` / `ctx.shell_streaming` from Lua. No Rust
file-snapshot code is needed.

**Scope guard (important):** only snapshot when the turn may touch files. A
`before_turn` hook that commits the whole working set on every turn (including
"what's the weather") is wasteful and implies a restore promise it can't keep.
Gate the snapshot on whether file-modifying tools are enabled/likely that turn.
Be honest in `/undo` output about what was restored vs. what wasn't (e.g. shell
side effects).

## 6. The Lua plugin

Lives in `~/.bone-rust/lua/` (or registered from `init.lua`). Three parts:

### 6a. `before_turn` snapshot hook

```lua
bone.on("before_turn", function(ctx)
  -- only when file tools may run this turn
  if not turn_may_touch_files(ctx) then return end
  local conv = ctx.conversation.current()
  if not conv then return end
  shadow_commit(ctx, conv.id)              -- git add -A; git commit
  local hash = shadow_head(ctx, conv.id)
  local key = ("undo:%s:turn:%s"):format(conv.id, ctx.turn_index or "?")
  ctx.state.set(key, hash)                 -- maps turn -> commit
end)
```

`turn_index` should be derived from `ctx.conversation.history()` length (count of
user messages). If `before_turn` doesn't currently expose a turn index, derive it
from history in the handler.

### 6b. `/undo` command — the tree picker

```lua
bone.register_command("undo", {
  description = "rewind chat (and files) to a prior turn",
  handler = function(arg, ctx)
    local conv = ctx.conversation.current()
    if not conv then return "No active conversation." end
    local hist = ctx.conversation.history()        -- current active path

    -- build candidate rewind points = user-message boundaries
    local turns = turn_boundaries(hist)             -- {idx, preview, commit_hash}

    -- render + choose via the interactive picker
    local choice = ctx.ui.interact({
      type = "single_select",
      question = "Rewind to before turn:",
      options = vim-like option strings,            -- "Turn 3 — \"fix the bug\" (2 tools)"
    })

    local target = turns[choice]
    if not target then return "Cancelled." end

    -- 1. restore files from shadow git
    if target.commit_hash then
      shadow_checkout(ctx, conv.id, target.commit_hash)
    end

    -- 2. move the DB active_tip back (pure SQL via the new exec API)
    ctx.db.exec(
      "UPDATE conversations SET active_tip_id = ? WHERE id = ?",
      { target.tip_message_id, conv.id }
    )

    -- 3. swap in-memory transcript + rebuild scrollback via conversation.load
    local truncated = slice_active_path(hist, target.idx)
    return {
      output = ("Rewound to before turn %d. Files restored."):format(target.turn),
      submit = false,
      action = "conversation.load",
      messages = truncated,
      conversation_id = conv.id,
    }
  end,
})
```

Notes:
- Use `action = "conversation.load"` (not `conversation.replace`): the load path
  calls `rebuild_scrollback_from_transcript`, so the screen matches the rewind.
  `replace` (`mod.rs:229`) keeps stale scrollback and only appends a marker —
  wrong for undo. (Optional later Rust tweak: make `replace` also rebuild.)
- The SQL `UPDATE` needs the target message's `id` as the new tip. Get it from
  `ctx.db.query("SELECT id, parent_id FROM messages ...")` walking the active
  path, or carry it from the history build.

### 6c. Tree rendering (optional, the "tree that displays")

The tree is queryable directly from the DB:

```sql
SELECT id, parent_id, role, substr(content,1,40) AS preview, seq
FROM messages WHERE conversation_id = ? ORDER BY seq;
```

Render with `ctx.ui.pane({ title=..., content=<lines> })`, marking the active
path (walk `active_tip_id` → root) and branch points. This is a read-only view;
selecting a node can reuse the `/undo` logic to switch tips.

## 7. Phasing

**Tier 1 — minimum viable rewind (this plan's core).**
- v5 migration: `parent_id`, `active_tip_id`, backfill, read-path CTE.
- `ctx.db.exec` (DML guard) + FTS insert trigger + remove manual FTS insert.
- `before_turn` shadow-git snapshots (gated on file tools).
- `/undo` picker: shadow restore + tip move + `conversation.load`.

Result: rewind works, survives reload, files restored, main git untouched. Tree
is in the data but not yet rendered.

**Tier 2 — tree view + branches.**
- `ctx.ui.pane` tree renderer, active-path highlighting, click-to-switch-tip.
- "Fork from here": start a new branch without losing the current one (already
  free given parent_id — just keep the old tip reachable).

**Tier 3 — branch management.**
- Named branches (a `branches` table or a `label` column on messages), branch
  list/switch command.
- GC: prune old, unreferenced branches + their shadow commits on demand (never
  automatic). The FTS delete/update triggers from §4b matter here.

## 8. Limitations & honest scope (document in `/undo` help)

- Rewound **files** are restored (via shadow git). Rewound **shell side effects**
  (installs, servers, `rm`, network) are **not**. `/undo` output should state
  what was and wasn't restored.
- Only the working tree tracked by shadow git is restorable; files never added by
  a tool turn, or excluded by `.gitignore`, won't snap back.
- Rewind is refused while a turn is streaming (only valid when idle).
- Shadow git grows with usage; provide a `/undo gc` (Tier 3) rather than silent
  eviction.

## 9. Open questions

1. **`before_turn` payload**: does the event carry a turn index / the pending
   prompt, or must the handler derive it from `ctx.conversation.history()`?
   Verify before writing the hook (search `dispatch_before_turn` in
   `src/ext/engine.rs` / `ctx.rs`).
2. **`conversation.replace` scrollback**: decide whether to keep using
   `conversation.load` for undo (works today, resets token stats) or add the
   small Rust change so `replace` rebuilds scrollback (lighter, no stat reset).
3. **Turn boundary detection for tool-call chains**: confirm the rule "rewind
   target = a user message, never inside an assistant turn" is enforceable purely
   from `history()` roles. It should be (user messages delimit turns), but verify
   against a real multi-tool transcript.
4. **Shadow git ignore handling**: decide whether the shadow repo inherits the
   project `.gitignore` (yes — `git add -A` does) and whether to add an
   `~/.bone-rust/undo/.gitignore` overlay for agent-internal noise.
5. **Concurrency**: `before_turn` runs off the UI thread; confirm `ctx.state` and
   `ctx.shell` are safe there (the compaction path already uses this pattern, so
   likely yes).
