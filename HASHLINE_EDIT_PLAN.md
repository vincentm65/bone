# Hashline Edit System — Adoption Plan

## Goal

Adopt OMP-style hashline edits: read-derived `[path#hash]` anchors + line-number
operations, replacing the "copy the old text" search/replace model with an
"edit these exact displayed lines from this exact file snapshot" model.

## Why

Current `edit_file` matches copied text against the live file. Robustness gaps:

- No mandatory stale-file protection (`expected_hash` is optional/manual).
- Fuzzy recovery may land edits in a "close enough" region.
- No visible-line provenance — model can target lines it never saw.
- No multi-file, line-range, delete, or block edits.
- Large edits require re-typing old content (token cost).

OMP's hashline model fixes these by binding edits to a content tag from the most
recent read and applying changes by original line number.

Reference: OMP `docs/tools/edit.md`, `packages/hashline/src/prompt.md`,
`packages/hashline/src/snapshots.ts`, `packages/coding-agent/CHANGELOG.md`.

---

## Key design decision: Option B (additive)

Add a **new `edit` tool** with hashline semantics. Keep `edit_file` as the
search/replace fallback during migration.

| Option | Pros | Cons |
|---|---|---|
| A. Replace `edit_file` | One blessed path | Big migration risk; weak for tiny text edits |
| **B. Add `edit` tool (chosen)** | Safer rollout; old path stays; gradual migration | Two edit systems temporarily; model may pick wrong one |

The new tool lives in `core/src/tools/edit/` (mirroring `edit_file/`).

---

## Scope tiers

### MVP (Tier 1) — safe anchored single-file edits
- Snapshot store in core
- `read_file` emits `[path#tag]` + `N:line` prefixes (model-facing)
- New `edit` tool: parse + apply single-file patch
- Ops: `SWAP N.=M:`, `DEL N.=M`, `INS.PRE N:`, `INS.POST N:`,
  `INS.HEAD:`, `INS.TAIL:`
- Tag validation; **reject** stale tags (no recovery yet)
- Visible-line guard
- Atomic write, permissions preserved
- Unified diff result + fresh `[path#tag]`

### Tier 2 — stale recovery
- Snapshot lookup on tag mismatch
- 3-way merge: apply intended patch onto current file
- Conflict detection; reject unsafe merges
- Recovery warning in result

### Tier 3 — multi-file + lifecycle integration
- Multiple `[path#tag]` sections in one `edit` call
- Preflight all sections before any write
- Mid-batch failure reporting (no rollback — like OMP)
- `write_file` mints fresh snapshot + header on success
- Web/TUI render multi-file edits

### Tier 4 — structural / advanced (optional, later)
- `SWAP.BLK N:`, `DEL.BLK N`, `INS.BLK.POST N:` via tree-sitter
- `MV dest` (rename) and `REM` (delete file)
- Lenient parsing of common malformed model output
- No-op loop guard
- Streaming preview

---

## Work breakdown

### 1. Snapshot store  *(Tier 1)*
**File:** `core/src/tools/edit/snapshot.rs` (new)

Session-scoped store. Per path holds:
- normalized full file text
- 4-hex content tag (`compute_file_hash`)
- timestamp
- seen-lines set (1-indexed lines actually displayed)
- last N versions (default 4) for recovery

Interface (mirror OMP `SnapshotStore`):
```rust
trait SnapshotStore {
    fn head(&self, path: &str) -> Option<&Snapshot>;
    fn by_hash(&self, path: &str, hash: &str) -> Option<&Snapshot>;
    fn by_content(&self, path: &str, text: &str) -> Option<&Snapshot>;
    fn record(&mut self, path: &str, text: &str, seen_lines: Option<&[usize]>) -> String;
    fn record_seen_lines(&mut self, path: &str, hash: &str, lines: &[usize]);
    fn invalidate(&mut self, path: &str);
    fn relocate(&mut self, from: &str, to: &str);
    fn clear(&mut self);
}
```

In-memory LRU default. Stored on the session/agent context so it survives across
tool calls within a turn but is per-session.

Tag: `sha256` → truncate to first 4 hex chars, uppercase. Collisions tolerated
(keyed by full text in the store).

### 2. Read format change  *(Tier 1)*
**File:** `core/src/tools/read_file.rs`

When the new edit tool is registered/enabled, `read_file` model-facing output:

```text
[src/foo.rs#A1B2]
41:fn greet(name: &str) {
42:    println!("hi");
43:}
```

Before returning content:
1. read full file (already done)
2. normalize (LF, strip BOM) — keep a copy for the snapshot
3. `record(path, normalized_text, seen_lines)` → tag
4. format header `[path#TAG]`
5. prefix each line `N:`

Decisions:
- Always hashline-format when `edit` is enabled? Likely yes, gated by tool
  registration so headless/non-edit sessions keep plain output.
- UI/TUI display: strip prefixes in the human view; model sees prefixes.
- Raw/range reads: record full-file snapshot when possible; mark only the shown
  range as seen-lines. Elided regions are not editable.
- Files > cap (OMP: 4 MiB) or unreadable: no tag, plain output.

### 3. Hashline parser  *(Tier 1)*
**File:** `core/src/tools/edit/parser.rs` (new)

Parse one `input` string into sections + ops.

```
Patch := Section+
Section := Header Op+
Header := "[" Path "#" Tag "]"
Op := Swap | Del | InsPre | InsPost | InsHead | InsTail
Swap := "SWAP" Range ":" Body
Del := "DEL" Range
InsPre := "INS.PRE" Int ":" Body
InsPost := "INS.POST" Int ":" Body
InsHead := "INS.HEAD:" Body
InsTail := "INS.TAIL:" Body
Range := Int ".=" Int | Int      // N.=M inclusive; bare N = N.=N
Body := ("+" Line?)*
```

Strict grammar, but tolerate a few safe variants (OMP-style):
- `SWAP N:` → `SWAP N.=N:`
- `DEL N` single-line
- missing trailing colon
- legacy `N..M` / `N-M` / `N…M` separators
- `*** Begin Patch` / `*** End Patch` envelopes stripped

Reject explicitly with teaching errors:
- apply_patch sentinels (`*** Update File:` etc.)
- unified-diff `@@` hunks
- `-old` rows
- body under `DEL`
- overlapping ranges on same file
- empty body under body-bearing op

### 4. Apply engine  *(Tier 1)*
**File:** `core/src/tools/edit/apply.rs` (new)

Apply parsed ops to a single file's text by original line number.

Rules:
- Line numbers refer to the **original** snapshot, not shifted per hunk.
- Sort hunks; reject overlaps.
- Build new content in memory (byte vector / string).
- If result equals input → no-op (return message, do not write).
- Validate each hunk's anchor lines were in `seen_lines` (visible-line guard).
- One atomic write via existing `write_atomic`; preserve permissions.

Reuse existing `write_atomic` and `edit_file/diff.rs` for the returned diff.

### 5. Edit tool  *(Tier 1)*
**File:** `core/src/tools/edit/mod.rs` (new)

Tool definition:
```json
{
  "name": "edit",
  "input": {
    "input": "string  // one or more [path#tag] sections with ops"
  }
}
```

Flow:
1. parse `input` into sections
2. for each section:
   a. read live file → normalize → compute live tag
   b. if live tag == submitted tag: apply ops to live text
   c. if mismatch (Tier 1): **reject** with "file changed; re-read"
   d. (Tier 2): attempt snapshot recovery
3. preflight all sections; atomic-write each
4. record new snapshots; return `[path#NEWTAG]` + unified diff per file

Register in `core/src/tools/mod.rs` alongside `edit_file`.

### 6. Prompt + AGENTS.md  *(Tier 1)*
Update `core/defaults/AGENTS.md` and any edit-related system prompt:
- read first, copy `[path#TAG]`, edit by line number
- body rows are `+TEXT`
- ranges are inclusive original lines
- one hunk per range; body = final content, never old/new pair
- do not edit unseen/elided lines
- on stale tag: re-read

### 7. Tests  *(Tier 1)*
**File:** `core/tests/edit_test.rs` (new)

Groups:
- parser: valid/invalid syntax, tolerated variants, rejections
- apply: SWAP/DEL/INS, multi-hunk original-line semantics, overlap rejection
- tag validation: match applies, mismatch rejects
- visible-line guard: reject unseen-line anchors
- snapshot recording from read_file
- atomic write, permission preservation, CRLF/BOM normalization
- no-op detection
- error message quality (teaching errors)

### 8. Stale recovery  *(Tier 2)*
**Files:** `core/src/tools/edit/recovery.rs` (new), extends apply/tool

On tag mismatch:
1. `base = snapshot_store.by_hash(path, tag)` — reject if none
2. apply ops to `base` → `edited_base`
3. diff `base → edited_base` (the intended change)
4. attempt to merge that change onto `current`
5. if target region unchanged in current: apply → write + recovery warning
6. if target region diverged: reject, tell model to re-read

No fuzzy/auto-relocate (OMP removed that as unsafe). Recovery only proves a
result when the edited region itself is unchanged in the live file.

### 9. Multi-file + lifecycle  *(Tier 3)*
- parse multiple sections
- preflight all before writes (like OMP)
- `write_file` records snapshot and emits `[path#tag]` on success
- TUI `tool_display` + webui render multi-file edits
- mid-batch write failure: report written vs unwritten paths (no rollback)

### 10. Structural + polish  *(Tier 4, optional)*
- tree-sitter block ops (needs grammar deps — cost/benefit review)
- `MV` / `REM` file ops
- lenient recovery of common malformed output
- no-op loop guard (after N byte-identical no-ops → hard error)
- streaming preview in TUI

---

## What stays unchanged
- `write_atomic` helper
- `edit_file/diff.rs` unified-diff generation
- existing `edit_file` tool (fallback during migration)
- approval/command-policy plumbing (edit is a `Danger` op like `edit_file`)
- expected-hash concept (subsumed by mandatory tag in the new tool)

---

## Risks & open questions

1. **Read format breakage.** Adding `[path#tag]` + `N:` prefixes changes what
   the model sees on every read. Gate behind edit-tool registration; keep a
   plain mode for non-edit sessions and for UI display.

2. **Snapshot storage lifetime.** Per-session in-memory is simplest. Persistence
   across sessions is **not** needed for MVP (OMP is per-session too).

3. **Tag collisions.** 4-hex = 16-bit space. Store keys by full text; collisions
   only affect the visible tag index, not correctness. Acceptable per OMP.

4. **Migration of existing prompts/tests.** Any prompt that teaches search/replace
   must also teach hashline. Decide per-prompt whether to switch.

5. **Two-tool confusion.** Model may pick `edit_file` when `edit` is better, or
   vice versa. Mitigate via system-prompt guidance; consider deprecating
   `edit_file` search/replace after Tier 2 is stable.

6. **Block ops cost.** Tree-sitter adds a dependency. Defer to Tier 4; line-range
   edits cover most cases.

---

## Suggested order

1. Snapshot store + tests (no tool wiring yet)
2. `read_file` format change behind a flag + tests
3. Parser + apply engine + tests (pure functions, no I/O)
4. `edit` tool single-file Tier 1 + tests
5. AGENTS.md / prompt updates
6. Manual end-to-end check in TUI
7. Tier 2 recovery
8. Tier 3 multi-file + write_file integration
9. Tier 4 as needed

---

## Success criteria

- A read followed by a `SWAP` edit lands on the correct line, first try.
- Editing a stale file (changed between read and edit) is rejected, not
  silently applied to drifted content.
- Edits targeting lines not in the read output are rejected.
- Multi-hunk edits apply with original (not shifted) line semantics.
- No regression in existing `edit_file` tests.
