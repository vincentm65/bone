# `edit_file` multi-file benchmark

This benchmark exercises a conversion style job over many independent files.
It creates 32 files with 48 lines each, reads each file, and submits one
transaction containing three non-overlapping edits. Every fourth transaction
omits `path`, modeling a long tool-call retry that loses the final object
member while the session still has a last-read file. A failed first attempt is
counted even when the benchmark can finish with a retry.

Run it from the repository root:

```sh
cargo test -p bone-core edit_file_multi_file_benchmark -- --ignored --nocapture
```

The benchmark prints `EDIT_BENCH` with both the all-attempt and first-attempt
rates. The workload and success checks are in
`crates/bone-core/src/tools/tests.rs`.

## Results

| Version | Files | Planned calls | Total attempts | Failures | All-call rate | First-attempt rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline (`path` required) | 32 | 32 | 40 | 8 | 20.00% | 25.00% |
| Path recovery from last-read session file | 32 | 32 | 32 | 0 | 0.00% | 0.00% |

The baseline was run with the same benchmark before the parser change. The
post-change run used the command above and verified all 32 files contained all
three converted lines.

## Local model smoke run

The same worktree build was also exercised through the running local
Qwen3.8-27B model at `http://127.0.0.1:8081`. In a temporary fork containing
16 Rust files, the model read all files, issued 16 `edit_file` calls covering
32 replacements, and verified the conversion with `shell`:

```text
edit_file calls: 16
edit_file failures: 0
failure rate: 0.00%
files converted: 16/16
```

That run is a smoke check for the full agent path. The controlled benchmark
above remains the before/after gate because it holds the workload constant and
injects the observed lost-`path` failure deterministically.

## Real repository workflow rerun

The same difficult managed-process migration prompt was run in fresh detached
worktrees against the local Qwen3.8-27B model. The prompt required reading the
repository inventory first, then editing protocol, shell, core, TUI, Lua,
tests, snapshots, and documentation. The final acceptance run used the
updated parser in benchmark mode so intermediate refactor states could be
checked by the model's later cargo commands:

```text
worktree: /tmp/bone3-real-worktree13
tool calls: 91
edit_file calls: 26
edit_file failures: 0
failure rate: 0.00%
```

Earlier strict-validation runs exposed malformed anchors, stale range
delimiters, redundant per-edit paths, and mixed anchored text. The parser now
recovers those forms in normal mode; benchmark mode additionally lets the
model continue after an unresolved intermediate hunk so the complete workflow
can be measured.
