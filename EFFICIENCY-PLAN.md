# Bone efficiency improvement plan

Status: **plan only** — nothing in this plan has been implemented. Written for a
different agent to execute after approval.

Scope: `~/projects/bone` (and sibling `~/projects/bone-bench` for validation).
Do not modify `~/.bone-rust` unless explicitly requested. No commits or pushes.

## 1. Evidence this plan rests on

Repo bench `~/projects/bone-bench/repo-bench/results-repo/20260914-201119`,
task `bone-run-json` (add `--json` to `bone run`), n=1, both harnesses PASS,
5/5 functional checks, final suite 0 failures.

| metric | bone | pi |
| --- | --- | --- |
| wall clock | 372.9 s | 176.5 s |
| tool calls | 41 | 18 |
| turns | 34 | 15 |
| edits | 12 `edit_file` | 4 `edit` (one call carried 8 edits) |

Recorded scores for this and every other completed run live in
`~/projects/bone-bench/scores/SCORES.md` (`scores.sh --save` appends new ones);
raw results are archived in `scores/archive/`.

Recomputed from saved events:

- bone call→turn histogram `{1: 27, 2: 4, 3: 2, 0: 1}` — overwhelmingly one tool
  call per turn.
- All 12 bone `edit_file` calls returned `is_error=false`: no retries, genuinely
  one hunk per call.
- bone's 41 calls decompose as 5 search (`find`/`grep`), 8 reads, 12 edits,
  10 `cargo build`/`test`, 6 misc.
- `bone/out.jsonl` and `agent-events.jsonl` are identical; `--events` emits no
  `tool_output` events in the headless path, so shell output content is not
  recoverable from the record.

## 2. Verified root causes

### 2.1 `edit_file` is single-hunk

`core/src/tools/edit_file/mod.rs:28-61`: `Args { path, old_text, new_text }` with
`#[serde(deny_unknown_fields)]`, `required: ["path","old_text","new_text"]`, and
a description that says "Replaces one exact, unique block".

pi's `edit` takes `{path, edits:[{oldText,newText}]}` and its guidance says
"When changing multiple separate locations in one file, use one edit call with
multiple entries in `edits[]` instead of multiple edit calls."

### 2.2 Batching works but is never advertised

- `driver.rs:1429-1439` calls `execute_tool_calls`, which runs approved calls
  **concurrently** (`driver.rs:1743`; `registry.rs:443`).
- bone already emitted 2- and 3-call turns, so the plumbing is proven
  end-to-end.
- But the base prompt (`core/defaults/config.yaml:3-7`, four lines) has **no
  tool-usage guidance**, tool descriptions say nothing about multi-call turns,
  and AGENTS.md is silent.
- `driver.rs:466,900-902` `turn_nudge` is a *user steer* channel, not an
  automatic efficiency nudge.

### 2.3 No search builtin — **not a differentiator**

`builtin_tools()` (`core/src/tools/mod.rs:90-96`) = `read_file, create_file,
edit_file, shell`. pi also spent 5 shell calls on `find`/`grep`; its `grep`,
`find`, and `ls` tools are not in pi's default tool selection.

### 2.4 Status chatter is **not** a cost (hypothesis falsified — do not "fix")

`RuntimeEvent::Status "running {tool}: …"` (`driver.rs:1358-1361`) →
`emit_event` (`agent.rs:235-343`) → print/frontend only. Status never enters
`history`. 42 events, zero round trips.

### 2.5 Read-before-edit is a deliberate tradeoff

`edit_file/mod.rs:115-124` requires a prior `read_file` snapshot; the error text
reads "read `{path}` with read_file before editing it". pi's `edit` has no such
guard (pi read 1 file all run; bone read 8). Keep this.

### 2.6 Verification churn is partly a bench artifact

~7 of bone's calls chased `catalog_async_test` / `catalog_e2e_test`, which
**pass at baseline and in the final gate** but failed intermittently mid-run
under the sandbox, prompting `git stash` A/B attribution. Both were re-run in
the real environment during planning: `cargo test -p bone-core --test
catalog_async_test --test catalog_e2e_test` → 1 passed each. Cause is
sandbox intermittency, not a regression.

## 3. Workstream A — multi-hunk `edit_file` (primary)

**Why:** 12 of 41 calls are single-hunk edits; pi collapsed 8 edits into one
call. Largest single lever, mechanical to implement.

### Interface

Add optional `edits: [{old_text, new_text}]`.

- Schema: `required` becomes `["path"]`; `old_text`/`new_text` remain as the
  legacy single-hunk form; add `edits`.
- Reject mixing the two forms.
- Keep `additionalProperties: false`.

### Semantics

- Each hunk is matched against the **original** normalized text
  (non-incremental — not the intermediate result).
- Each hunk must be unique (`unique_match_offset`) and visible
  (`ensure_visible` per hunk).
- Reject overlapping and duplicate hunks.
- Apply by mapping every normalized offset to raw via `raw_offset`, splicing
  right-to-left, in one `write_atomic_if_unchanged`.
- Generalize `remap_seen_lines` from 1 hunk to N, keeping the
  `unchanged_suffix_continues` rule per hunk.
- Reuse `build_unified_diff` on the whole file.
- Share the apply logic between `preview_edit_file` and `run_edit`.
- **Do not change the `"Edited: {path}"` first line** — `tui/src/ui/render/messages.rs:182`
  splits on a trailing `" ("` for a summary suffix.

### Touch points

| file | items |
| --- | --- |
| `core/src/tools/edit_file/mod.rs` | `Args`, `parse_args:169-180`, `run_edit:102-167`, `preview_edit_file:84-100`, `unique_match_offset:182-204`, `replace_unique:206-213`, `raw_offset:215-236`, `replace_raw:238-253`, `ensure_visible:255-281`, `remap_seen_lines:283-317` |
| `core/src/tools/edit_file/diff.rs` | `build_unified_diff`, `build_numbered_diff_lines` — reusable as-is |
| `core/src/tools/snapshot.rs` | `Snapshots`, `Snapshot{text,format,tag,seen_lines}`, `record_with_format:124-156`, `normalize_text:167-183`, `numbered_lines:188-190`, `TextFormat::restore_newlines:57-64` |
| `core/src/agent.rs:674-698` | `summarize_call_args` keys on `path` — safe with `edits[]` |
| `tui/src/ui/tool_display.rs:105`, `tui/src/ui/render/messages.rs:182`, `tui/tests/tool_display_test.rs:101` | output parsing; unchanged if the header line is preserved |

### Tests

Add: multi-hunk apply; original-not-intermediate matching; overlap rejection;
mixed-form rejection; empty `edits`; CRLF/BOM preservation; per-hunk visibility
error; **follow-up edit on a region produced by a previous edit succeeds without
re-read**.

Update:

- `core/tests/edit_file_test.rs:355-359` `schema_has_only_the_three_simple_fields`
  asserts `schema["required"] == ["path","old_text","new_text"]` and
  `additionalProperties == false`.
- `core/tests/tool_args_guard_test.rs:44-59` asserts the empty-object guard
  message contains `path`, `old_text`, `new_text` (built from
  `registry.rs:181-232 required_fields()`); `:79-113` truncated-args and
  `populated_arguments_still_reach_the_tool` reference `edit_file`.

### Expected effect

12 edit calls → ~3; 41 → ~32 calls; 34 → ~26 turns; far fewer diff blobs in
history.

## 4. Workstream B — advertise batching (primary, cheap)

**Why:** concurrency plumbing already exists and bone already emitted
multi-call turns; it is simply never advertised.

### Change

Add a fixed guidance block in `core/src/llm/prompts.rs::system_prompt`
(chosen over editing `core/defaults/config.yaml` because seeding is
create-if-missing, so a YAML prompt change would **not** reach existing
installs). Content, phrased conservatively:

- "You may issue multiple independent tool calls in a single turn; batch
  related reads/searches instead of one per turn."
- "`edit_file` accepts several disjoint replacements in one call via `edits`."
- "Do not re-read a file you just changed unless the edit reported the file
  changed."

Also mention `edits` in the `edit_file` description for discoverability.

### Tests

`core/src/llm/prompts_tests.rs`: the case named
`configured_prompt_gets_only_runtime_context_appended` asserts the prompt prefix
and `!contains("You are bone")`; add assertions for the guidance block.

### Expected effect and risk

4–8 fewer turn round trips. **Risk:** the local Qwen3.8-27B may ignore prose
guidance — treat as unproven until measured.

### Callers of `system_prompt` (unchanged interfaces)

`tui/src/main.rs:215`, `core/src/agent.rs:465`, `core/src/runtime/session.rs:402`,
`core/src/rpc/mod.rs:1512,1812,1886,2367`. Subagents use
`headless_agent_system_prompt` (`prompts.rs:26-48`), which already carries
richer tool rules.

## 5. Workstream C — verification churn (optional, low confidence)

The real fix is bench-side: pre-fail-list `catalog_async_test` and
`catalog_e2e_test` in `bone-bench` so the agent is not tempted to investigate a
known-flaky sandbox failure. A secondary note in
`core/defaults/docs/development.md` is honest but low value.

State plainly: the flaky-test detour was partly *legitimate diligence*, not pure
waste.

## 6. Explicit non-goals

| non-goal | why not |
| --- | --- |
| Remove status-event chatter | Falsified: UI-only path, never enters `history` |
| Add a `grep`/`glob` builtin | Not a differentiator; pi did the same via bash |
| Relax read-before-edit | Deliberate safety tradeoff; keep |

## 7. Separate from efficiency (Phase-2 review nits)

- Keep `AgentResponse.transcript` in the result type.
- Build the envelope with `serde_json::json!`.
- Move the plain-vs-JSON branch into core so both paths are unit-tested.

## 8. Validation protocol

Prerequisites (verified present at planning time): llama.cpp on
`127.0.0.1:8081`, pi 0.73.1, warm shared target `repo-bench/shared-target`
(~15G), ~1.5T free on `/home`.

1. Implement A + B with tests in `~/projects/bone`.
2. Rebuild the harness binary: `cargo build -p bone` →
   `target/debug/bone` (the default `BONE_BIN`).
3. Re-run the repo bench with the base commit **pinned** to the original:
   `bench-repo.sh --base e1e8b5689799df0f98bfa63f3a977eed849eae4b --skip-warm`.
   Pinning matters: `--base` defaults to repo `HEAD`, which moves once the
   changes land, and `BONE_BIN` is a separate path from the worktree, so the
   improved binary is measured against the same base tree.
4. `bench-repo.sh` has **no `--runs` option** — one invocation is one run per
   harness in a fresh timestamped dir. For n=3, invoke it three times (the
   original run was n=1); `scores.sh` aggregates every dir it finds.
5. Save the scores: `~/projects/bone-bench/scores.sh --save` (appends a dated
   snapshot to `scores/SCORES.md`).
6. Targets: calls 41 → ~20; wall 373 s → ~200 s; tests green; same diff shape;
   0 new clippy warnings. For a cheaper A-only iteration, run
   `--harness bone` first, then the full two-harness run.
7. B is unproven until the re-run shows a turn-count drop.

## 9. Optional follow-ups from the bench

- Repo-bench task 2: `list_files` tool with a glob filter in `core/src/tools/`,
  registered, with tests.
- Phase-1 tasks t05–t09: t05 subtle interaction bug (~300–500 LOC) whose obvious
  one-line fix fails a hidden assertion; t06 implement-to-spec with 15–20 edge
  assertions; t07 long-horizon multi-objective; t08 confusing-failure debugging
  with a misleading traceback; t09 behavior-preserving refactor under a
  regression suite.
- Clean up bench worktrees / shared target when done:
  `git -C ~/projects/bone worktree remove …`.

## 10. Assumptions / unverified

- Whether the local Qwen3.8-27B obeys new prose guidance (Workstream B) —
  unmeasured.
- Exact cause of the intermittent `catalog_*` failures under sandbox isolation
  (not named in the record; `--events` captures no tool output).
- Whether either harness used seeded default Lua extensions/tools beyond
  builtins in the repo bench.

## 11. Artifacts

- Scores (durable record): `~/projects/bone-bench/scores/SCORES.md` (recorded
  table, append-only snapshots via `scores.sh --save`) and
  `~/projects/bone-bench/scores/archive/*.tar.gz` (raw results for the initial
  runs, with sha256 in `SCORES.md`). `~/projects/bone-bench/scores.sh` prints
  and saves the table for every results dir it finds.
- Bench: `~/projects/bone-bench/repo-bench/{bench-repo.sh,check.sh,task.txt}`;
  options `--timeout S`, `--harness bone,pi`, `--base <commit>`, `--skip-warm`,
  `-h`; env `REPO`, `SHARED_TARGET`, `BONE_BIN`, `PI_BIN`. Note: no `--runs`.
- Per-harness results: `meta.json, out.jsonl, agent-events.jsonl, err.log,
  check/{baseline-*,build.log,test.log,diff.patch,diffstat.txt,numstat.txt,
  status.txt,json.out,plain.out,jsonevents.out,help.out}` plus `review.md`.
- Shared target `~/projects/bone-bench/repo-bench/shared-target` (reusable with
  `--skip-warm`). Worktrees kept at
  `worktrees/20260914-201119/{bone,pi}`.
- Phase 1: `~/projects/bone-bench/{run.sh,summarize.sh,tasks/t01..t04,README.md}`,
  results `results/20260914-184155`.
