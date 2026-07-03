# Core tool improvements

Review of the builtin tools (`core/src/tools/`): `read_file`, `write_file`,
`edit_file`, `shell`, plus `command_policy`. The toolset itself is the right
minimal core — nothing here adds a tool or removes one. The changes fall into
three groups: output-budget gaps, a schema diet for `edit_file`, and targeted
robustness fixes. Each item states the reasoning, then the concrete change.

Considered and rejected (kept here so we don't re-litigate):

- **`workdir` param on shell.** Models handle `cd path && cmd` fine. A second
  way to say the same thing adds schema surface on every turn, a new failure
  mode (nonexistent workdir), and a classification question. Doesn't pay rent.
- **Line numbers in `read_file` output.** The failure mode (line prefixes
  leaking into `edit_file` search text and breaking exact match) is worse than
  the problem. The `[showing lines X–Y of N]` trailer is already the right
  amount of position awareness.
- **Command-position-only danger classification.** Any-token matching is what
  catches `xargs rm`, `find -exec rm`, `echo pw | sudo -S`. Rebuilding it
  position-aware means enumerating every forwarding construct, and each miss
  is a hole. See item 6 for the narrower fix that removes most false positives
  without loosening anything.
- **`bash -lc` → `bash -c`.** Login profiles are where users' PATH
  (`~/.cargo/bin`, nvm, pyenv) comes from. Changing this trades profile-source
  time for confusing "command not found" failures. Leave it.

---

## 1. Byte-cap shell output (high priority) — DONE

**Reasoning.** `truncate_output` (`shell.rs:128`) truncates by *line count*
(500 stdout / 100 stderr) with no per-line or total byte cap. A command that
emits one minified multi-MB line — `curl` of a bundle, `jq -c`, a base64
dump — sails through untouched into the context window. This is exactly the
bug class `read_file` already fixed with `MAX_LINE_CHARS`; shell is the same
failure one tool over, and shell output is the more common source of huge
lines.

**Change.** In `truncate_output`, cap each line the same way
`read_file::truncate_line` does (~2000 chars, truncate on a char boundary,
append `…[truncated]`). Extract the existing `truncate_line` helper from
`read_file.rs` into a shared location (e.g. `tools/mod.rs` or `util`) rather
than duplicating it. Keep the existing head/tail line-count behavior on top.
Add a test: single 10 MB line in, output under ~10 KB.

## 2. Size guard on `read_file` (high priority) — DONE

**Reasoning.** `execute` calls `fs::read_to_string` (`read_file.rs:92`)
before any slicing, so a ranged read of a multi-GB log still materializes the
whole file in memory and stalls the turn. Separately, a non-UTF-8 (binary)
file surfaces as the raw io error "stream did not contain valid UTF-8", which
models tend to retry instead of routing around.

**Change.** In `execute`, `fs::metadata` first and refuse files over a
threshold (50 MB is fine) with an instructive error:
`"file is N MB; too large to read directly — use shell (head/tail/rg) or a
narrower tool"`. Map the invalid-UTF-8 read error to
`"file is not valid UTF-8 (probably binary); use shell to inspect it"`.
Do the same metadata check in `execute_output` before the image path reads
bytes, so a 500 MB PNG doesn't get base64'd either (image threshold can be
lower, e.g. 10 MB — providers reject huge images anyway).

## 3. Slim the `edit_file` schema (high priority — biggest simplification) — DONE

**Reasoning.** The `edits[]` items accept four operation shapes — `search`+
`replace`, `delete`, `insert_before`+`text`, `insert_after`+`text` — plus a
`match` field whose enum has exactly one value (`"exact"`). Every variant is
sugar for search/replace, and `apply_one_operation`
(`edit_file/mod.rs:347`) already compiles all four down to a needle/
replacement pair internally:

- `delete: X` ≡ `search: X, replace: ""`
- `insert_before: A, text: T` ≡ `search: A, replace: T + A`
- `insert_after: A, text: T` ≡ `search: A, replace: A + T`

The variants cost real money: four shapes documented in the tool description
(longer prompt every turn), the "exactly one operation kind" validation dance
in `parse_operation` (~75 lines), the `text`-as-`replace` tolerance hack that
exists *because* models confuse the shapes, and a wider decision space for
the model to get wrong. Claude Code's edit tool is `old_string`/`new_string`/
`replace_all` and is sufficient at scale.

**Change.**
- **Schema** (`definition()`): `edits[]` items expose only `search`,
  `replace`, `replace_all`. Remove `delete`, `insert_before`, `insert_after`,
  `text`, and `match` from the advertised schema. Rewrite the description
  string: three modes (single search/replace, `edits[]`, `mode=rewrite`), one
  operation shape.
- **Parser**: keep `RawEditOperation` accepting the old fields and keep
  `parse_operation` converting them, so old transcripts replay and models
  that emit the legacy shapes from habit still succeed (same
  accept-and-ignore posture as shell's legacy `classification` field). The
  removal is from the *advertised* schema, not the parser. `match` likewise:
  accept, only reject non-`"exact"` values.
- Update/keep existing tests for the legacy shapes so the tolerance doesn't
  silently rot.

## 4. Hide `expected_hash` from the model-facing schema

**Reasoning.** `expected_hash` exists for the host's preview→approve→execute
flow (`preview_edit_file` returns `before_hash`; the host injects the hash on
execute). Advertising it in `input_schema` invites a model to hallucinate a
SHA-256 and earn a confusing "file changed since preview" failure. The model
never has a legitimate source for this value.

**Change.** Delete the `expected_hash` property from `definition()`'s
`input_schema`. Keep the field in `Args` — the host round-trip still uses it.
Before landing, verify the approval flow injects the hash into the arguments
host-side rather than expecting the model to echo it (grep for callers of
`preview_edit_file` / `before_hash`).

## 5. Truncate the returned diff for large edits — DONE

**Reasoning.** `execute_edit_file` returns the full unified diff. For
`mode=rewrite` of a 3k-line file the model gets a ~3k-line diff echoed into
the transcript — content it just wrote. The summary line already carries the
signal.

**Change.** In `execute_edit_file` (and `preview_edit_file` if the preview
pane wants it too — TUI preview may legitimately want the full diff, so only
cap the *tool result* string), cap the diff body at ~200 lines, keeping head
and tail with a `... N lines omitted ...` marker (reuse `truncate_output`
from item 1). Always keep the `edited file (+N, -M)` summary line intact.

## 6. Stop classifying quoted tokens as command names

**Reasoning.** `command_name` (`command_policy/mod.rs:285`) strips quotes
before matching, so `grep "rm -rf" src/`, `git log --grep=kill`, and
`rg 'dd\('` all contain a "danger command" token and prompt. False prompts
are the real security cost: they train the user to mash approve. The fix must
*not* loosen the any-token net (that's what catches `xargs rm`); it only
needs to stop treating string literals as commands.

**Change.** In `classify_segment`, when deriving `names` from tokens, skip
tokens that begin with `"` or `'` (they are arguments, not command words —
a shell never resolves a quoted word alone as... it does, but a model writing
`"rm" -rf /` is not a realistic evasion we need to hold against the
*approval prompt*; danger mode and the user gate still exist). Concretely:
in `command_name`, return an empty string (filtered out) for tokens whose
first char is a quote, instead of trimming the quotes off. Keep everything
else identical. Add tests: `grep "rm -rf" src` → ReadOnly (given grep is in
read_only policy), `xargs rm` → Danger, `find . -exec rm {} \;` → Danger.

Also fix two fragile substring checks while in the file:
- `curl`/`wget` download detection uses `command.contains(" -O")`
  (`mod.rs:184`) which matches URLs and unrelated flags. Match against parsed
  tokens: any token equal to `-O`, `-o`, or `--output`(-prefix), or a `>`
  redirect (already covered by the redirect check).
- `command.contains("| tee")` misses `|tee`. Check the parsed segment names
  for `tee` instead (shell_segments already splits on `|`).

## 7. Fuzzy-match pre-filter in `edit_file`

**Reasoning.** On the miss path, `fuzzy_candidate` (`edit_file/mod.rs:516`)
normalizes and runs `normalized_levenshtein` over ~3 windows per file line.
A 20k-line file with a 60-line needle is tens of thousands of multi-KB
comparisons — and this fires exactly when the model is already retrying a
failed edit. Acceptance requires score ≥ 0.92, which is unreachable when the
window's length differs from the needle's by more than ~8%.

**Change.** In `fuzzy_candidate`, before normalizing a window, skip it when
`|window.len() - needle.len()|` exceeds ~10% of `needle.len()` (byte lengths
are a fine proxy; keep the margin slightly looser than the score bound to
account for normalization shrink). This changes no accepted match, only
skips ones that could never pass. Add a perf-shaped test or at least a unit
test confirming a known fuzzy match still resolves.

## 8. Small fixes — DONE

- **Signal in shell exit reporting.** `exit code: signal` (`shell.rs:201`)
  hides which signal. On Unix use
  `std::os::unix::process::ExitStatusExt::signal()` and report
  `exit code: killed by signal 9`. SIGKILL frequently means OOM — actionable
  for the model.
- **Cache `shell_command()`.** It's called per-execution and per-definition,
  and on Windows `which("pwsh")` *spawns pwsh each time* (`shell.rs:46`).
  Wrap the detection in a `OnceLock`.
- **`write_file` symlink/TOCTOU.** `path.exists()` (`write_file.rs:57`) races
  with the atomic rename, and a *dangling* symlink returns `false` then gets
  clobbered by the rename. Use `fs::symlink_metadata` (catches dangling
  symlinks) for the check. The remaining race window is acceptable for this
  tool's threat model; note it in a comment.
- **`register_lua_tools` O(n²) clone.** `loaded.registry =
  loaded.registry.clone().register(tool)` (`tools/mod.rs:60`) clones the
  whole registry per tool. Add a `&mut self` registration method (or a batch
  `register_all`) on `ToolRegistry`. Only matters as the catalogue grows;
  do it opportunistically.

---

## Suggested order

1. Item 1 (shell byte cap) + item 2 (read_file size guard) — same bug class,
   shared helper, immediate context-budget protection.
2. Item 3 (edit_file schema diet) + item 4 (`expected_hash`) — one PR, all
   schema-only with tolerant parsing; verify with existing edit_file tests
   plus new legacy-shape tests.
3. Item 6 (command_policy quoted tokens + substring fixes) — pure
   false-positive reduction, well covered by table tests.
4. Items 5, 7, 8 — independent, any order.
