# `edit_file` diagnosis: two recurring errors

Status: **diagnosis complete, no fixes applied.** Every claim below is traceable to a
source location or to a row in the session database (message ids are cited).

Scope: the multi-hunk `edits` form of `edit_file` introduced in `f1fe038`
("Fix edit_file snapshot visibility remapping", 2026-09-14 21:14:05 -0400).

---

## 1. Summary

| # | Error the model sees | Real cause | Confidence |
|---|---|---|---|
| 1 | `edit_file requires path plus either old_text/new_text or a non-empty edits array: missing field \`path\`` | The model emits the large `edits` array **first** and `path` **last**, and a copy/paste re-send can drop `path`. The serde error is opaque and the tool header shows no filename. | High — byte-level evidence |
| 2 | `old_text was not found in \`<path>\`; copy it exactly from read_file` | One hunk of five diverged by **a single character** (a stray `:`); matching is all-or-nothing and reports no hunk index, no matched-count, no near-miss. | High — byte-exact diff vs `git show HEAD:` |

Three secondary defects multiply the cost of both failures: the stale-file check runs
*after* the visibility check, complete-but-invalid JSON is reported as "truncated", and
`preview_edit_file` errors are discarded with `.ok()`.

Failure rate on the days covered by the local database: **~11.3 %** of `edit_file` calls
(39 / 344), and **~19 %** for multi-hunk calls versus **7.3 %** for single-hunk calls.

---

## 2. What changed (`f1fe038`)

`git show --stat f1fe038` — `core/src/tools/edit_file/mod.rs` (+283 / −82), plus prompts
and tests.

The commit:

- added the `edits: [{old_text, new_text}, …]` array form;
- relaxed the tool schema to `"required": ["path"]`, so the *only* structurally required
  argument is the small one that is sent last;
- made matching all-or-nothing (`match_hunks` anchors every hunk on the original text and
  bails on the first error).

So the array-of-many-hunks call — the shape this commit was written to encourage — is now
the shape with the worst failure rate, and it is also the shape that can lose `path`.

---

## 3. Failure statistics

Source: `~/.bone-rust/data/conversations.db`, all 344 `edit_file` calls on 2026-09-19 and
2026-09-20 (`/tmp/diag_stats.py`, `/tmp/diag_hunkstats.py`).

```
date        calls  failed  rate
2026-09-19    105      12  11.4%
2026-09-20    239      27  11.3%

by shape (argument form):
  edits    calls=186 failed=26 (14.0%)
  old/new  calls=158 failed=13 ( 8.2%)

by hunk count:
  hunks  calls failed  rate
    1     232     17   7.3%
    2      46     10  21.7%
    3      32      5  15.6%
    4      11      0   0.0%
    5       7      3  42.9%
    6       9      2  22.2%
    7       4      1  25.0%
    9       3      1  33.3%
```

Multi-hunk overall ≈ **19 %** versus **7.3 %** for a single hunk: the failure rate is
roughly proportional to the number of hunks, exactly as all-or-nothing matching predicts
(one bad character in one hunk fails the whole call).

Error kinds observed:

```
   13  old_text includes lines that were not shown from ...
    8  old_text was not found in ...
    6  read ... with read_file before editing it
    3  edit_file requires path plus either old_text/new_text or a non-empty edits array
    2  (empty tool error)
    1  provide either old_text/new_text or edits, not both
    1  no change to ...
    1  old_text occurs more than once in ...
    1  edits hunks overlap in ...
    1  bulk read matched no visible regular files for ...
    1  could not resolve ...
    1  tool call arguments were truncated (6860 bytes of incomplete JSON ...)
```

**Caveat:** the database only retains 2026-09-19 and 2026-09-20, i.e. *after* `f1fe038`.
A true before/after baseline could not be measured. The claim "this got worse recently"
rests on the shape/hunk-count split above and on the commit itself, not on a measured
regression.

---

## 4. Error 1 — ``missing field `path` ``

### 4.1 What the user saw

Conversation 40. Assistant message **7069** issued a 5-hunk `edit_file` call; the tool
reply is message **7070**:

```
edit_file requires path plus either old_text/new_text or a non-empty edits array: missing field `path`
```

The TUI header for that call renders as `✕ edit_file` with **no path**, because
`core/src/agent.rs:679` (`summarize_call_args`) summarizes `edit_file` as
`arguments["path"]`, which is absent.

### 4.2 The arguments were complete and valid

Message 7069's arguments are valid JSON, 3610 bytes, and contain **no `path` key at all**:

```json
{"edits":[ …5 hunks… ]}
```

It parses. It was not truncated. The only problem is the missing key — which is exactly
the key the schema marks as required (`core/src/tools/edit_file/mod.rs`, schema block
~lines 66–103: `"required": ["path"]`, top-level `additionalProperties: false`).

### 4.3 Why the model drops it: emission order

Schema key order in the `tool_calls` column is not evidence — it is re-serialized through
an alphabetically ordered map. The only reliable record of emission order is the raw
stream text preserved in the truncation marker.

Message **6428** (role assistant, conversation 40, 2026-09-20T16:42:29Z) contains the
`__bone_truncated_args__` marker holding 6860 bytes of raw emitted arguments. Its tail is:

```
        );\n    }\n}"}], "path": "/home/vincent/projects/bone-bench/src/lib.rs"}
```

That is the literal stream text: the model closes the **`edits` array first** and emits
`"path"` **last**.

Consequence: `path` is (a) emitted last, therefore (b) the first thing lost to a truncation
and (c) the most likely field to be dropped in a hand-rolled re-send of a long call.

### 4.4 Frequency

Across the 344 calls on 2026-09-19/20, only **4 lack `path`**:

| msg | timestamp (UTC) | note |
|---|---|---|
| 3354 | 2026-09-20T01:56:43Z | |
| 5784 | 2026-09-20T15:47:54Z | |
| 6428 | 2026-09-20T16:42:29Z | truncation marker (see §7.2) |
| 7069 | 2026-09-20T17:40:58Z | the user-visible incident |

Three of four fall inside the last two hours of the log, at the moment the model was
issuing large multi-hunk calls. Low absolute count, but the error message is uninformative
(`missing field \`path\`` does not say "send `path`" or where), and in 7069 it cost a full
extra round trip: the model re-sent the identical 5 hunks with `path` restored (message
7071) and hit error 2 again.

---

## 5. Error 2 — ``old_text was not found``

### 5.1 What the user saw

Message **7060** (conversation 40, 17:39:5xZ) issued a 5-hunk `edit_file` against
`/home/vincent/projects/bone/tui/src/ui/app/mod.rs`. Tool reply **7061**:

```
old_text was not found in `/home/vincent/projects/bone/tui/src/ui/app/mod.rs`; copy it exactly from read_file
```

The same message appears again at **7072** and **7080**.

### 5.2 The decisive evidence: one stray character

`/tmp/diag_head.py` compares each hunk of messages 7060 and 7071 byte-for-byte against
`git show HEAD:tui/src/ui/app/mod.rs`:

```
== message 7060 args json bytes: 3490
  hunk 0: len=164  in HEAD=1  -> matches HEAD exactly
  hunk 1: len=191  in HEAD=1  -> matches HEAD exactly
  hunk 2: len=408  in HEAD=0  -> differs from HEAD at 407
                                model: ':'   head: '\n'
  hunk 3: len=152  in HEAD=1  -> matches HEAD exactly
  hunk 4: len=127  in HEAD=1  -> matches HEAD exactly
```

The identical result is reproduced for message 7071. Four of five hunks match the file
**exactly** (this also proves the model's anchoring was otherwise correct, and that the
"clipped-looking" endings of the other hunks — `…(Windows conhost`, `…when a history entry`
— were genuinely exact, not truncated). Hunk 2 diverges at its **last character**: the
model wrote `/// Collect all available slash commands with descriptions: native builtins:`
(with a trailing colon) where the file has `...native builtins` (no colon).

One stray colon failed the entire call.

### 5.3 What the tool told the model about it

`unique_match_offset` (`core/src/tools/edit_file/mod.rs:246-268`) is the *only* producer of
this message, and `match_hunks` (lines 273–308) calls it per hunk with `?`, so:

- no hunk index;
- no count of how many hunks matched;
- no indication of *where* the text diverged;
- no near-miss suggestion.

The message actively misleads: `copy it exactly from read_file` implies the model copied a
whole block wrongly. In reality 4/5 blocks were perfect and one had an extra character at
the end — which the model could not find by re-reading, as the recovery loop shows.

Note `tui/src/ui/app/mod.rs` has a maximum line length of 123 characters, so
`MAX_TOOL_LINE_CHARS = 2000` (`core/src/tools/mod.rs:30`) and `truncate_line` played no
part here.

### 5.4 The observed recovery loop

Conversation 40, 17:39:25Z → 17:42:41Z (messages 7058–7082), reconstructed with
`/tmp/diag_timeline.py`:

| # | message(s) | action | result |
|---|---|---|---|
| 1 | 7058 | single-hunk `edits` call | Edited |
| 2 | 7060 → 7061 | **5-hunk call** | `old_text was not found in …` |
| 3 | 7062–7068 | re-reads 4–5 ranges of the file (2005–2024, 2947–2991, 3290–3321, 2390–2407, 2408–2425) | cannot locate a 1-char error |
| 4 | 7069 → 7070 | re-sends the **same 5 hunks, drops `path`** | `missing field \`path\`` |
| 5 | 7071 → 7072 | same 5 hunks, `path` restored | `old_text was not found in …` |
| 6 | 7073/7075/7077 → 7074/7076/7078 | abandons multi-edit; three single-hunk calls | all Edited |
| 7 | 7079 → 7080 | reuses the same bad `…native builtins:` anchor | `old_text was not found in …` |
| 8 | 7082 | re-reads lines 2974–3003 | … |

Roughly **25 messages and ~3 minutes of wall clock** to land five changes, for a single
stray character. The final workaround is degenerate: the model stops using `edits` and
issues one call per hunk, i.e. the feature is abandoned under failure.

---

## 6. Secondary findings (code reading)

### 6.1 Stale-file check runs after the visibility check

`run_edit` (`core/src/tools/edit_file/mod.rs:143-200`):

1. lines 154–165: snapshot lookup, then `ensure_visible` for every hunk — produces
   `old_text includes lines that were not shown from \`{path}\`; read that range before editing`;
2. lines 167–171: **then** the live-vs-snapshot comparison — produces
   ``\`{path}\` changed after it was read; re-read it and retry``.

Both the snapshot text and the live file are already in hand before the checks, so the
staleness test can run first. As written, if the file legitimately changed after the read
*and* a hunk touches a region that is not visible, the model is told
"copy it exactly from read_file" — the wrong instruction, sending it to re-read and retry
the same call. Error kind `read … with read_file before editing it` (6 occurrences) and the
13 `not shown` failures are the family this ordering muddles.

### 6.2 Complete-but-invalid JSON is reported as truncation

`core/src/llm/provider.rs:24-30`:

```rust
serde_json::from_str(raw).unwrap_or_else(|_| json!({ TRUNCATED_ARGS_KEY: raw }))
```

Any parse failure — including a *complete* payload with a syntax error — becomes the
truncation marker. Then `core/src/tools/registry.rs:183-233`
(`reject_degenerate_arguments`) emits:

> `tool call arguments were truncated ({n} bytes of incomplete JSON, likely the output-token limit); do not resend the same call — split the work into smaller edits with the required field(s): …`

Verified against message 6428 (`/tmp/diag_trunc.py`): the payload is **6860 bytes, ends
with a proper `}` and contains `"path"`** — it is *not* truncated. Python reports
`Invalid control character at: line 1 column 5773 (char 5772)`, and `'\n' in raw` is `True`:
the model emitted a **literal newline inside a JSON string**. The advice given
("do not resend the same call") is the opposite of correct — a properly escaped resend
would have worked. The raw marker is also persisted into conversation history as if it were
the assistant's arguments.

### 6.3 `preview_edit_file` errors are discarded

`preview_edit_file` (`core/src/tools/edit_file/mod.rs:127-141`) surfaces the parse/match
failure before approval. Both call sites throw the error away with `.ok()`:

- `core/src/runtime/event.rs:260` (ChannelApprovalGate)
- `core/src/ext/ctx.rs:2903` (jobs / agent watcher)

So an approval prompt can show a broken or empty diff with no reason, and the one place
that could have explained the failure silently reports nothing.

### 6.4 `not shown` failures cluster on windowed reads

The 13 `old_text includes lines that were not shown from …` failures concentrate on
`bone-bench/src/lib.rs` (messages 6837, 6874, 6978, 6989, 7000 — all 17:32–17:36Z) and on
`crates/core/flags/*` (4290, 5229). Pattern: a windowed `read_file`, then a multi-hunk
`edits` call reaching into ranges that were never displayed. The visible-range ledger
works as designed; the problem is that a multi-hunk call can be mostly-invisible and the
error names neither the offending hunk nor the missing range.

---

## 7. Planned fixes

Ordered by value/risk. None are implemented yet.

### Fix 1 — per-hunk diagnostics in `match_hunks` / `unique_match_offset`

`core/src/tools/edit_file/mod.rs:246-268` and `:273-308`.

This is the highest-value change: it directly converts the §5 loop into one round trip.

- Thread the hunk index into the error: `edits[2] of 5: old_text was not found in …`.
- Report how many hunks matched: `4 of 5 hunks matched; the failing hunk is #2 (offset 0 of …)`.
- Add a bounded near-miss hint: if the first line of `old_text` is found, report the first
  differing character with ~40 characters of context on each side — the same
  "did you mean" shape already used by `path_repair` for `resolve_existing_path`.

Expected effect on the reported incident: `model: '…native builtins:' head: '…native builtins\n'`
would have made the stray colon obvious immediately.

### Fix 2 — run the staleness check before `ensure_visible`

`core/src/tools/edit_file/mod.rs`, `run_edit` lines 154–171: hoist the
`if base != live || format != live_format` test above the `for hunk in &hunks { ensure_visible(...) }`
loop. A file that changed since the read must say
``changed after it was read; re-read it and retry`` rather than
`copy it exactly from read_file`.

Small, self-contained, no behavior change in the non-stale path.

### Fix 3 — separate invalid JSON from truncation

`core/src/llm/provider.rs:24-30` plus `core/src/tools/registry.rs:183-233`.

- Distinguish *unterminated* (truncation is plausible) from *syntax error at position N*
  (payload is complete but malformed).
- Carry the parse position and a short excerpt into the steer.
- Only tell the model "do not resend the same call" when the payload really is truncated;
  for an invalid-but-complete payload, say to resend with correct escaping (e.g. no literal
  newlines inside strings).
- Stop persisting the raw marker as if it were the assistant's arguments.

### Fix 4 — `path` hygiene

`core/src/tools/edit_file/mod.rs` (schema + description) and `parse_args` (~line 202).

- Make the missing-field failure name the field and the fix: replace the generic
  `format!("edit_file requires path plus either old_text/new_text or a non-empty edits array: {e}")`
  wrapper with an explicit check that produces, e.g.,
  ``edit_file is missing `path`; send `path` first, before the `edits` array``.
- Nudge emission order in the property description ("send `path` first").
- Optionally accept an `edits`-only payload and return the targeted steer instead of a
  serde error, since the rest of the arguments are otherwise usable.

### Fix 5 — surface `preview_edit_file` errors

Replace `.ok()` at `core/src/runtime/event.rs:260` and `core/src/ext/ctx.rs:2903` with
propagation, so the approval prompt shows a one-line reason instead of a missing diff.

### Suggested sequencing

Fixes 1–3 address the two reported errors and the misleading-advice defect. Fixes 4–5 are
cheap hardening. Recommended: 1, 2, 3 first, then 4 and 5.

---

## 8. Reproducing the evidence

The session database is `~/.bone-rust/data/conversations.db` (`sqlite3` at `/usr/sbin/sqlite3`).
Tables: `conversations`, `messages`, `usage_events`, `conversation_context_checkpoints`.
`messages` columns: `id, conversation_id, role, content, tool_name, tool_call_id,
tool_calls, images, is_error, payload_json, seq, created_at`.

Assistant `tool_calls` is JSON `[{"id","name","arguments":{…}}]`; its key order comes from an
alphabetically ordered map and is **not** evidence of emission order. Only the raw string
inside a `__bone_truncated_args__` marker preserves the order the model emitted.

Scripts used (all in `/tmp`, re-runnable):

| script | purpose |
|---|---|
| `diag_stats.py` | per-day call/failure counts, split by argument shape, error-kind tally |
| `diag_hunkstats.py` | failure rate by hunk count |
| `diag_timeline.py` | message-by-message reconstruction of 7058–7082 |
| `diag_head.py` | byte-exact hunk comparison vs `git show HEAD:tui/src/ui/app/mod.rs` |
| `diag_trunc.py` | truncation-marker payload validation (length, tail, `json.loads` position) |
| `diag_edit.py`, `diag_hunks.py`, `diag_diff.py`, `diag_payload.py`, `diag_notshown.py`, `diag_lines.py` | supporting queries |

Note: Bone's destructive-command guard rejects several shell forms (backticks, `>` inside
SQL/heredoc text, `$var` inside redirection targets). Writing a Python file with
`create_file` and running `python3 /tmp/script.py` works reliably.

---

## 9. Open questions / not investigated

- No before/after baseline: the database retains only 2026-09-19 and 2026-09-20, both
  post-`f1fe038`, so the regression size for the multi-hunk form is unmeasured.
- Whether `path`-last emission is stable across models/providers, or specific to the model
  used here, was not tested.
- The 2 empty tool errors and the `could not resolve …` / `bulk read …` failures were
  counted but not traced; they are outside these two errors.
- Working tree currently has unrelated uncommitted changes
  (`core/src/shell_split.rs`, `core/src/shell_split_tests.rs`,
  `core/src/tools/command_guard.rs`, `core/src/tools/command_guard_tests.rs`,
  `core/src/tools/shell.rs`, `tui/src/ui/app/mod.rs`); they are not part of this diagnosis.
