# Simple File Tools Plan

## Goal

Replace the agent-facing hashline editing language with a small, predictable
interface that weaker models can use by copying text they just read.

## Tool contracts

### `read_file`

Input remains:

```json
{"path":"src/main.rs","start_line":1,"max_lines":200}
```

Relative paths resolve from the session working directory. The tool returns a
stable normalized path, a short range summary, and numbered text. Revision and
visibility data are recorded internally and are not exposed as syntax the
agent must repeat.

### `edit_file`

Input becomes:

```json
{
  "path": "src/main.rs",
  "old_text": "let value = 1;",
  "new_text": "let value = 2;"
}
```

`old_text` must occur exactly once in the latest content shown by `read_file`;
it may be empty only when the file itself is empty. An empty `new_text` deletes the match. Insertions are
expressed by copying a small unique surrounding block into both fields and
adding text to `new_text`.

## Safety and behavior

1. Resolve read and edit paths through the same normalizer and key snapshots by
   that normalized path.
2. Require a prior `read_file` in context-aware execution.
3. Reject replacements outside the ranges actually shown to the agent.
4. Reject missing or ambiguous `old_text` with an actionable re-read message.
5. If the file changed after the read, apply the exact replacement only when it
   is still uniquely present in the live file; otherwise reject the edit.
6. Preserve file permissions, write atomically, and return a unified diff.
7. Keep image reads, paging, UTF-8 validation, line truncation, and size limits.

## Implementation steps

1. Add shared path normalization for existing files.
2. Change `read_file` output and descriptions while retaining internal snapshot
   recording.
3. Replace hashline parsing in `edit_file` with the three-field exact-replace
   schema and implementation.
4. Adapt approval summaries, diff previews, built-in callers, documentation,
   and UI assumptions.
5. Replace hashline-specific tests with simple replacement, deletion,
   insertion, ambiguity, visibility, stale-file, and path-resolution tests.
6. Run formatting plus the core, TUI, and Web UI test suites; remove obsolete
   hashline modules after all references are gone.
