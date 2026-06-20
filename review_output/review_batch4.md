# Review Batch 4 — `/home/vincent/projects/bone/src/tools/`

---

## /home/vincent/projects/bone/src/tools/mod.rs
- **Lines:** 156
- **Assessment:** mostly good
- **Notes:** Clean facade re-exporting all tool modules. `ApprovalMode` (enums with `as_u8`/`from_u8`, `cycle`, `label`, `mode_str`, `allows_safety`, `allows_call`) and `SharedApprovalMode` (atomic wrapper with `get`/`set`) live here but logically belong in `approval.rs` — that module is only 89 lines and could absorb them, removing the cross-file dependency from `approval.rs` into `mod.rs`. `LoadedTools` is a simple bag struct used only at startup; could be inlined or moved to `registry.rs`. The `load_tools()` function lacks a `?` / error path despite `register_lua_tools` being fallible by design (name conflicts). Otherwise clean.

---

## /home/vincent/projects/bone/src/tools/approval.rs
- **Lines:** 89
- **Assessment:** mostly good
- **Notes:** Well-factored module extracting the approval decision into a pure function (`decide_call`) with no dependencies on `ToolHandler` or `ExtensionManager`. The `ApprovalGate` trait with default impl and `AutoApprovalGate` zero-struct is a clean seam for interactive frontends. `denied_message` centralizes the error string. However, `ApprovalMode` and `SharedApprovalMode` live in `mod.rs` while they are the inputs to this module's functions — moving them here would make the module self-contained. Minor: `CallOutcome` has `Blocked(String)` carrying a reason, but the other two variants carry no data; consider whether `Blocked` vs `Denied` distinction is always needed by callers (it is used at least in the TUI for display).

---

## /home/vincent/projects/bone/src/tools/read_file.rs
- **Lines:** 61
- **Assessment:** mostly good
- **Notes:** Simple, focused, minimal. One struct (`ReadFileTool`), one `Args` deserialization, one `execute` method. No over-engineering. The `max_lines` cap (1000) is hardcoded in both the schema and the execute method as a `.min(1000)` — the schema already enforces it via `maximum`, so the runtime clamp is defensive duplication. Could drop `.min(1000)` since the deserialization will reject values > 1000, but it's harmless.

---

## /home/vincent/projects/bone/src/tools/registry.rs
- **Lines:** 367
- **Assessment:** over-engineered
- **Notes:** `ToolRegistry` (60 lines) is clean: simple `HashMap<String, Arc<dyn Tool>>` with `register`, `definitions`, `execute_live`. The over-engineering is in `ToolHandler` (~270 lines), which conflates too many responsibilities:
  - Tool enable/disable filtering (with `HashSet<String>`)
  - Dynamic display config lookups (`HashMap<String, ToolDisplayConfig>`)
  - Dynamic safety lookups (`HashMap<String, CommandSafety>`)
  - Session state management (delegating to `ToolStateMap`)
  - Cancellation token ownership (`Arc<AtomicBool>`)
  - App ctx snapshot propagation (`AppCtxState`)
  - Parallel vs serial execution decisions (host-stateful tool detection with `is_host_stateful_name`, `host_state_key_for_name`)
  - Session state override tracking in `execute_all_serial` via a local `state_overrides: HashMap`
  - Owner string propagation
  - Two constructors (`new` and `with_enabled_safety_and_display`)
- The serial-vs-parallel logic in `execute_all` checks `filter(|call| Self::is_host_stateful_name(&call.name)).count() > 1` — this only looks at `> 1`, meaning a single host-stateful call still goes through `join_all`. The `execute_all_serial` method then maintains `state_overrides`, which is stateful tracking baked into the handler rather than in `ToolStateMap` where it belongs.
- `execute_one_live` handles slash-commands, disabled-tool errors, and delegation to the registry — three concerns in one method.
- Suggestion: split into (1) a thin `ToolHandler` that delegates execution, (2) a separate `ExecutionPlanner` for serial/parallel logic, (3) remove dynamic display/safety from handler and query them directly from the registry or a dedicated config store.

---

## /home/vincent/projects/bone/src/tools/shell.rs
- **Lines:** 187
- **Assessment:** can be simplified
- **Notes:** Three concerns in one file: (1) `ScriptRequest`/`ScriptOutput`/`run_script` — a generic script execution engine, (2) `truncate_output` — output formatting, (3) `ShellTool` — the tool implementation. The `run_script` function opens a subprocess, reads stdout/stderr concurrently with `tokio::try_join!`, applies a timeout via `tokio::time::timeout`, and truncates output. `shell_command()` is called on every invocation to get the shell name; it could be cached with `OnceCell`/`OnceLock`. The `which()` helper checks for `pwsh` vs `powershell` on every `shell_command()` call. The `_` ignoring of `shell_label` in `run_script` suggests the triple return from `shell_command` is wasteful. `truncate_output` could be a free function in a utilities module. The `classification` field in `Args` is received from the model but explicitly ignored (`let _ = args.classification;`) with a comment that `command_policy` is the sole authority — if the model field is never used, consider removing it from the schema and the struct to avoid confusion.

---

## /home/vincent/projects/bone/src/tools/state_map.rs
- **Lines:** 37
- **Assessment:** mostly good
- **Notes:** Minimal, focused, correct. Simple `HashMap<String, HashMap<String, String>>` with `set`, `get`, `remove`, `clear`. The doc comment mentions "source" and "sub_key" but only `task_list`/`default` is used today. Clean as-is. No simplification needed.

---

## /home/vincent/projects/bone/src/tools/types.rs
- **Lines:** 113
- **Assessment:** mostly good
- **Notes:** Core type definitions are clean and well-documented. `ToolExecutionContext` has 11 fields, which is large but each field has a distinct purpose (call_id, session_state, owner, cancelled, agent_depth, tool_call_depth, tool_handler, app_state). The `Tool` trait has a default method chain: `execute` → `execute_output` (default wraps execute) → `execute_output_live` (default wraps execute_output). This is a reasonable hierarchy for incremental feature addition. `ToolDisplayConfig` with `serde(default)` on all fields is clean. `ToolLiveEvent` only has one variant (`Key(KeyRequest)`), which suggests a single-use enum — could be a type alias, but the enum allows future variants. Fine as-is.

---

## /home/vincent/projects/bone/src/tools/write_atomic.rs
- **Lines:** 48
- **Assessment:** mostly good
- **Notes:** Single-purpose atomic write utility. Uses a temp file with `create_new(true)` and renames. The error handling cleans up the temp file on each failure path, but this is repeated in 4 places (`map_err` closures) — a helper like `with_temp_cleanup` or a `defer`-style pattern could reduce duplication. The temp file name encodes `pid` and `nanos`, which is fine for uniqueness. The `permissions` parameter adds flexibility but is only used by `edit_file` — could be simplified if the caller always sets it. Minor: uses `tokio::fs` for I/O but falls back to `std::fs::remove_file` in error paths (synchronous) — inconsistent but not harmful.

---

## /home/vincent/projects/bone/src/tools/write_file.rs
- **Lines:** 63
- **Assessment:** mostly good
- **Notes:** Straightforward `WriteFileTool` implementation. Creates parent directories, checks for existing file (with a clear error message), then calls `write_atomic`. The "reject if exists" check is necessary because `write_atomic` uses `create_new` on the temp file (which would succeed even if target exists, then `rename` would silently overwrite on Unix). Could potentially merge the early-exists check into `write_atomic` as an option, but that would complicate the atomic writer's contract. Fine as-is.

---

## /home/vincent/projects/bone/src/tools/command_policy/mod.rs
- **Lines:** 413
- **Assessment:** over-engineered
- **Notes:** The largest and most complex non-edit module, doing too many things:
  - YAML policy loading with `OnceLock` caching, `load_command_policy`, `default_raw_command_policy`, YAML deserialization, normalization
  - Shell wrapper peeling: `peel_shell_wrapper`, `strip_command_prefix`, `peel_shell_args`, `unquote`
  - Command classification: `classify_command`, `classify_segment`, `shell_segments`, `command_name`
  - Dangerous command detection: `has_dangerous_git_command`, `has_non_dev_null_redirection`
  - Policy merging: `edit` + `package_managers` merged into `danger`
  - `DEFAULT_COMMAND_POLICY` embedded via `include_str!`
- The dangerous command list is extensive and ad-hoc: hardcoded check for `sed -i`, `awk` with `>`, `curl`/`wget` with download flags, `systemctl stop/restart`, `tee`, redirection to non-/dev/null paths, etc. Each is a special case that will need maintenance as new dangerous patterns emerge.
- `classify_segment` is 90+ lines with deeply nested `if let` + `matches!` patterns and early returns, making it hard to trace all classification paths.
- `peel_shell_args` attempts to parse shell flags (handling `-c`, `-Command`, `-CommandWithArgs`, `-NoProfile`, `-NonInteractive`, `-ExecutionPolicy`) — this is shell-specific logic that would be better in a dedicated parser or shell wrapper module.
- Suggestion: split into (1) `policy.rs` — YAML loading/caching/normalization, (2) `classifier.rs` — the core classification logic, (3) `shell_peel.rs` — the wrapper peeling. The hardcoded dangerous-command rules could be moved into the YAML policy so users can extend them without code changes.

---

## /home/vincent/projects/bone/src/tools/edit_file/mod.rs
- **Lines:** 597
- **Assessment:** over-engineered
- **Notes:** The largest file in `tools/` and the most complex. Contains:
  - `EditFileTool` struct + `Tool` impl (~40 lines)
  - Argument deserialization (`Args`, `RawEditOperation`)
  - Edit operation enum + parsing (`EditOperation`, `parse_operation`, `parse_operations`) (~120 lines)
  - Content building with validation (`build_candidate_content`, `ensure_no_edit_fields_for_rewrite`) (~60 lines)
  - Application of individual operations (`apply_one_operation`, `replace_matched_span`) (~30 lines)
  - **Fuzzy string matching** (`find_match_span`, `normalized_candidates`, `fuzzy_candidate`, `line_window_candidates`, `line_spans`, `needle_line_count`, `MatchSpan`, `Candidate`, `FuzzyCandidate`) (~200 lines)
  - Preview support (`preview_edit_file`, `EditPreview`)
  - Hash-based change detection (`sha256_hex`, `expected_hash`)
- The fuzzy matching subsystem (`fuzzy_candidate`, `normalized_levenshtein`, `normalized_candidates`) is the most over-engineered part. It normalizes whitespace, computes Levenshtein distance, builds line-window candidates, checks ambiguity margins, requires a minimum score of 0.92 and margin >= 0.08 and needle length >= 30 characters. This is ~100 lines of code for an edge case: when the model's edit anchor doesn't match exactly. For an LLM agent, it would be better to fail clearly (exact match only) and let the model retry with the exact text from the file. The fuzzy matching introduces the risk of silently editing the wrong location.
- `parse_operation` has complex "stray text field tolerance" logic: `"If `replace` is missing, treat `text` as the replacement so the edit can still work."` — this works around model quirks instead of requiring correct schema usage.
- Suggestion: extract the fuzzy matcher into its own module or remove it entirely in favor of exact-only matching. Split the rest into `edit_file.rs` (tool + preview), `parse.rs` (argument/operation parsing), and `apply.rs` (operation application).

---

## /home/vincent/projects/bone/src/tools/edit_file/diff.rs
- **Lines:** 98
- **Assessment:** mostly good
- **Notes:** Clean utility building unified diffs with line numbers using the `similar` crate. `build_numbered_diff_lines` manually parses unified diff output to produce formatted output with line numbers — this duplicates some of `similar`'s functionality. Could potentially use `similar::TextDiff::from_lines(old, new).unified_diff().context_radius(3).to_string()` directly and skip the custom line-number formatting, but the current output format (with right-aligned line numbers and `-`/`+` markers) is more readable. The `parse_hunk_header` function is manual `str` parsing where `similar`'s API might expose the hunk metadata directly (though `similar` 0.7+ may not). Minor: `summarize_change` calls `build_numbered_diff_lines` but only uses the counts, discarding the lines — wasteful; could compute insertions/deletions directly from a `TextDiff` iterator.
