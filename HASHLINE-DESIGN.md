# `hashline_edit` design

Status: implemented and benchmarked better than `edit_file` on failure rate,
latency, and tokens (see Results). Enabled unless listed in `tools.disabled`.

## Why

`EDIT-FILE-DIAGNOSIS.md` shows ~11 % of `edit_file` calls fail (19 % for
multi-hunk). Every dominant failure class comes from the model re-typing file
content as `old_text`:

- one stray character in one hunk fails the whole call (`not found`);
- duplicated blocks need extra context (`occurs more than once`);
- any change after the read (including the model's own earlier edit read
  through stale line numbers) forces a full re-read;
- `old_text` is pure overhead output tokens: the model pays to repeat text
  that already sits in its context.

## Format

With hashline enabled, `read_file` renders each line as

```
{line}#{hash}|{content}
```

`hash` is 2 chars from a 32-symbol alphabet (FNV-1a over the exact line
bytes, 1024 values). The prefix is no longer than today's `{n:>5} | `, so
reads cost the same tokens.

## Edit API

```json
{"path": "src/lib.rs", "edits": [
  {"at": "12#k3", "end": "14#ab", "text": "new line\nother"},
  {"at": "30#zz", "text": ""},
  {"after": "40#qq", "text": "inserted"},
  {"before": "1#aa", "text": "// header"}
]}
```

- `at` (+ optional inclusive `end`) replaces lines; `text: ""` deletes them.
- `after` / `before` insert without touching the anchor line.
- `text` lines are split on `\n`; one trailing `\n` is ignored.
- Anchors tolerate a copied `|content` suffix and whitespace.
- Every edit anchors on the same file state; overlaps are rejected; edits
  apply all-or-nothing.

## Anchor resolution (the robustness win)

1. `line#hash` matches the live file: use it.
2. Otherwise look for the most recent recorded version of the file (snapshot
   head, then bounded history of earlier reads/edits) where that line had that
   hash, map the line to the live file with an exact line diff (`similar`),
   and use it if the line is unchanged. This covers stale line numbers after
   the model's own earlier edits and unrelated external changes.
3. Otherwise fail with fresh anchors for the surrounding live lines so the
   model can retry without a re-read.

Ambiguity rule: a stale anchor is resolved against every recorded version
(the live file plus up to 8 history snapshots; snapshots where no edit's
anchors match are skipped before diffing). Two or more distinct live
hits, or a hit alongside a version where the line changed, is rejected as
`ambiguous`, and the error lists fresh anchors for each candidate. This
prevents silent mis-edits on low-entropy lines such as blanks or `}`.

A hash can only be learned from a read, so a matching anchor proves the line
was seen. The interior of a multi-line range must have been shown.

## Output

`Edited: {path} (-d +i)` followed, per changed region, by the new lines with
fresh anchors plus one context line on each side. Removed lines are not
echoed. Chained edits need no re-read.

## Rollout

`hashline_edit` is a builtin tool. Like every registered builtin, it is
enabled unless listed in `tools.disabled`; while it is enabled, `read_file`
switches to hashline rendering. To use it exclusively, add `edit_file` to
`tools.disabled`.

## Results

`core/tests/hashline_bench.rs` (`#[ignore]`) uses a simulated model with noise,
not a real LLM. Release build, 20 files × 5 seeds, all edit kinds, 3 runs:

| Tool | ok % | fail first try % | fail any attempt % | silent corruptions | reads | read tok | arg tok | out tok | total tok | ms/task |
|---|---|---|---|---|---|---|---|---|---|---|
| `edit_file` | 93.5 | 26.7 | 24.5 | 20 | 2.56 | 4964 | 304 | 458 | 5726 | 0.56–0.59 |
| `hashline_edit` | 99.5 | 1.0 | 1.4 | 0 | 1.79 | 3157 | 138 | 196 | 3491 | 0.51–0.54 |

Run it with
`cargo test -p bone-core --release --test hashline_bench -- --ignored --nocapture`.
Set `BENCH_DEBUG=1` to dump silent corruptions.

Open items: the TUI/native display has no hashline-specific rendering yet,
and it has not yet been validated with a real model.
