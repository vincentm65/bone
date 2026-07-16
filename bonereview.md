# bone-core Code Review

**Scope:** all of `core/` (src + tests, ~34k LOC); excludes `protocol/`. Read-only — no source files changed. Findings verified against source; `cargo check -p bone-core` passes.

**Method:** 5 parallel deep-audit subagents (ext, rpc/runtime, llm, tools, session_db/config) → every Critical/High claim was re-read against source, and **one over-claim was corrected** (engine native-exec; see §3).

Confidence tags: ✅ = read at the exact lines during this review; ◑ = subagent-reported, plausible but not re-read line-by-line.

---

## 1. Architecture assessment

bone-core is well-layered for its size, and the **Neovim split is sound**: `runtime/` owns the headless driver/session/event loop; `rpc/` is a clean JSON-line codec + session manager; `llm/` is a `Provider` trait with per-vendor impls; `tools/` is a `Tool` trait + registry with an approval/policy gate; `ext/` wraps an mlua VM as the scripting/config layer. The dependency direction is one-way (TUI/web → protocol → core), which is correct.

Structural concerns (all ◑ unless noted):

- **Approval is not enforced at the registry level** ✅ — `ToolRegistry::execute_live` (registry.rs:61-98) does no approval check; the gate runs *above* it in the driver. Any caller that invokes a tool directly — tests, `ctx.agent.spawn` from Lua, `ctx.shell_streaming` (see §3) — bypasses all policy. This is the single biggest architectural gap: **policy is a convention, not an invariant.**
- **Two-tier path resolution that diverges** ✅ — `snapshot::resolve_path` (sync, no canonicalization) vs `resolve_existing_path` (async, canonicalizes). `write_file` uses the unsafe one. See §3 C1.
- **Two execution paths per tool** (`execute` vs `execute_output_live`, types.rs:66-79) — headless `execute` callers skip snapshots/cancel/working-dir. Should collapse to one path.
- **Per-provider request building is duplicated ~4×** (codex / openai_compat / anthropic / grok), and all SSE errors map to `LlmErrorKind::Connection` unconditionally. A shared message→wire builder and a shared SSE error classifier would cut hundreds of lines.
- **`SessionDb` holds a raw `rusqlite::Connection` (`!Sync`)** (session_db.rs:382) with no type-level guard against concurrent `&self` access from two threads. Masked today by a single-threaded dispatch model; invisible to the type system.
- **Good:** no SQL injection anywhere — all user input flows through `params![]` ✅ (the `format!` calls only splice compile-time column constants); `http_error` caps response bodies at 2000 chars consistently across providers; streaming tool-call accumulation (`PartialToolCall`) is shared between codex/openai_compat.

---

## 2. Top issues by severity (verified)

### CRITICAL — security

**C1. `write_file` path traversal / working-dir escape** ✅
`snapshot::resolve_path` (snapshot.rs:22-32) joins relative paths to `working_dir` but **never canonicalizes and never checks containment**; `write_file.rs:81` calls it (not the canonicalizing `resolve_existing_path`). A model can create new files anywhere: `../../etc/cron.d/x`, `~/.ssh/authorized_keys`, absolute `/etc/...`. The "reject if exists" guard only blocks *overwrite*, not creation. This defeats the documented working-dir sandbox for writes.

**C2. Command substitution `$()` and backticks bypass classification** ✅
`shell_split.rs` does not parse `$()`, backticks, `${}`, globs, or brace expansion. `command_name` (command_policy/mod.rs:287) trims only `'"&(){}`, not `$` or `` ` ``. `classify_segment` matches token *names*; a **read-only wrapper** (`echo`/`cat`/`ls`/`printf`/`git status`/`cargo`) carrying a substitution is classified `ReadOnly` (line 244-264) while the shell runs the substituted command. The default-`Danger` fallback (272) catches unknown leads, but every allowlisted command is a hole. e.g. `cat "$(touch /tmp/pwned)"` → `ReadOnly`. (`sudo`/`rm` as *standalone tokens* are still caught — so the simplest exploits are the wrapper-substitution form.)

**C3. `ctx.shell_streaming` executes `bash -c` with no policy/approval/cancel** ✅
ctx.rs:517-523 spawns `Command::new("bash").arg("-c").arg(&command)` directly — bypassing the `run_script`/`ScriptRequest` pipeline that `ctx.shell` uses. Any Lua path that calls this (plugins, commands) runs arbitrary commands with no safety classification, approval, env restrictions, or cancellation. Also spawns **two OS threads per call** (ctx.rs:530, 548) instead of reusing the async streaming path.

**C4. `ctx.db.query` guard is trivially weak + cross-conversation read** ✅
ctx.rs:1112 — `sql_trimmed.to_lowercase().starts_with("select")` is the only protection. It's bypassable with a leading comment (`/* x */ select…`) or fooled by a CTE (`with…` is read-only but rejected), and a Lua script can `SELECT` **any** conversation's rows by guessing an id (no session scoping). *Whether multi-statement injection executes depends on the (unread) execute path* — the guard itself is the confirmed defect.

### HIGH — security / reliability

- **`setsid()` return ignored → potential wrong-process-group kill** ✅ shell.rs:106-109. `pre_exec` discards `setsid()`'s result; if it fails, the child isn't a group leader and `kill(-pgid)` can hit siblings. Check the return; fall back to child-only kill.
- **Codec accepts unbounded line length → OOM** ✅ codec.rs:44-66, `BufReader::lines()` with no cap. (Lower risk: the bridge is localhost-only, but a buggy/compromised client can still exhaust daemon memory.) Cap with `read_until` + size limit.
- **Null byte in command diverges classifier vs shell** ◑ shell.rs:96 — `command` is a Rust `String` that may contain `\0`; the classifier sees the full string, `bash` sees only the prefix. Reject `\0` up front.
- **TOCTOU in `edit_file`** ◑ edit_file/mod.rs:111-152 — canonicalize → … → `write_atomic_if_unchanged`; a symlink swap between them can redirect the write. Open a handle at resolve time (`O_NOFOLLOW`) or re-canonicalize pre-rename.
- **`ProcessRegistry::kill`/`list` ignore owner** ◑ processes.rs:88-107 — agent A can kill/enumerate agent B's jobs. Check `snapshot.owner`.
- **`openai_compat`: stream-usage URL match by substring** ◑ openai_compat/mod.rs:89-95 — `api.openai.com.phish.com` matches; `127.0.0.1` matches any local proxy. Match on parsed host exactly.
- **Anthropic prompt-cache header missing** ◑ anthropic.rs:281-288 — `cache_control` is sent but the required `anthropic-beta` header isn't, so caching silently never activates (pure cost/latency, not security).
- **Grok OAuth refresh holds tokio `Mutex` across `.await`** ◑ grok_build.rs:83-113 — a slow token endpoint blocks every concurrent turn. Clone refresh token → drop guard → refresh → re-lock.
- **Config migration deletes source on partial failure → data loss** ✅ custom.rs:636-694 — any page that fails `load_yaml` is `continue`d, then line 694 unconditionally `remove_file`s `config-values.yaml`. User's values for unparseable pages are gone forever.
- **Approval orphaned-senders leak** ◑ rpc/event.rs:119,132 + rpc/mod.rs:1290 — `oneshot::Sender`s registered in session-lived registries with no eviction; cancelled turns leak entries. Add a drop-guard/generation.

### Correction to subagent (over-claim)
**engine.rs "native code execution via `require`" is NOT a real exploit** ✅ — `package.loadlib` is stubbed to error (engine.rs:289-299), and the default C searcher calls `loadlib`, so `require`ing a `.so` fails safely. Real (low) items there: `package.cpath`/`debug` lib not explicitly hardened (defense-in-depth only).

---

## 3. Performance (verified/◐)

| Sev | Location | Issue |
|---|---|---|
| High | rpc/mod.rs:1148-1159 + session.rs:302 | `std::sync::Mutex` held across **full `transcript.clone()`** while building the driver — O(n) alloc under a lock contended by the runtime. Snapshot fields under a short lock, clone outside. |
| High | openai_compat/mod.rs:392+435 (635-636) | Every SSE chunk JSON-parsed **twice** (`delta_has_reasoning_field` then `process_sse_chunk`). Parse once, pass `&Value`. ◐ |
| High | runtime/driver.rs:596-599 | Cancel polled via `AtomicBool` every **25 ms** even in a biased `select!`; interactive Esc has up-to-25 ms lag. Use `tokio::sync::watch`/eventfd for instant wake. |
| Med | runtime/driver.rs:408, 275 | `tool_defs.clone()` every loop iteration — wrap in `Arc`. |
| Med | runtime/driver.rs:552-568 | Retry backoff fixed 2 s, **no jitter** — concurrent sub-agents thunder-herd the provider. |
| Med | session.rs:222-226 | `recompute_context_estimate` round-trips tool defs `to_value().to_string()` just to count chars — use `to_string` directly. |
| Med | session.rs:80-89 | `display_transcript()` clones full DB history each call; cache the snapshot. |
| Med | session_db.rs:1011-1058 | `usage_stats_snapshot` runs 17+ sequential prepared statements per call — gate behind explicit user action, never per draw. |
| Low | ext/catalog.rs:67-78 | `thread::spawn().join()` per remote fetch — use `spawn_blocking`. |
| Low | ext/api_ui.rs:235-242 | `term_width` queries crossterm each call, bypassing shared UI state — unify with `ctx.ui.width()`. |

---

## 4. Security & error-handling patterns (cross-cutting)

- **Pervasive silent error swallowing** ✅ — `.ok()` / `.unwrap_or_default()` / `let _ =` / `eprintln!` on I/O and parse errors across config (`load_yaml`, custom.rs:419-436), session_db (`now_iso` → epoch on clock skew, session_db.rs:1478-1484), and the Lua event dispatch. Typos in config keys and corrupted YAML silently reset to defaults with no warning. This is the dominant reliability risk.
- **Mutex poison always recovered** ✅ — every `Mutex::lock()` uses `.unwrap_or_else(|e| e.into_inner())`, so a panic that poisoned the lock lets all later callers run on **unknown state** (ctx.rs, types.rs, api_ui.rs, jobs.rs:157). At minimum `expect()` with context.
- **`eprintln!` is the only error channel** for Lua-layer failures — invisible to the UI/logs.
- No API-key leakage found in `http_error` (2000-char cap) ✅ — but codex.rs:581-584 surfaces raw error bodies that proxies may decorate; consider redacting `sk-…`/`xai-…` patterns before display.
- **No provider-level retry/backoff** for 429/5xx — entirely delegated to callers.

---

## 5. Clean code / DRY (representative)

- **build.rs:23-58** — `generate_default_lua_tools`/`_commands` are 95% identical; one helper eliminates the dup.
- **command_policy/mod.rs:370-376** — `contains_static_name`/`contains_config_name` are identical shapes differing only by `&str` vs `String`; parameterize over `AsRef<str>`.
- **session_db.rs:1133-1236** — five `WITH RECURSIVE … LEFT JOIN` bucket queries share one skeleton; extract a `gapfill_buckets` helper.
- **command_policy/mod.rs:120-273** — `classify_segment` (~160 lines, ~15 early returns) is order-fragile: the `read_only` early-return must stay *after* the danger checks or a refactor silently downgrades dangerous commands. Collect evidence into a struct, decide once.
- **shell.rs:394** — `reject_obvious_file_write` requires a space before `>`; `echo x">/etc/y` slips the first-line gate (the policy layer catches it later, but the two tiers disagree).
- **Dead/adjacent code**: `normalize_json_schema` (lua_tool.rs:250), unused `_lua` param (snapshots.rs:159).

---

## 6. Before → After (top 3 criticals)

### #1 — `write_file` working-dir containment (C1)

**Before** — `core/src/tools/snapshot.rs:22`
```rust
pub fn resolve_path(path: &str, working_dir: Option<&Path>) -> Result<PathBuf, String> {
    if path.trim().is_empty() { return Err("`path` must not be empty".into()); }
    let path = PathBuf::from(path);
    Ok(if path.is_relative() {
        working_dir.map_or(path.clone(), |cwd| cwd.join(path))   // no canonicalize, no containment
    } else {
        path                                                        // absolute paths allowed unconditionally
    })
}
```

**After**
```rust
pub fn resolve_path(path: &str, working_dir: Option<&Path>) -> Result<PathBuf, String> {
    if path.trim().is_empty() { return Err("`path` must not be empty".into()); }
    let joined = working_dir.map_or_else(|| PathBuf::from(path), |cwd| cwd.join(path));
    let canon = joined.canonicalize()              // resolves .., ., symlinks
        .map_err(|e| format!("could not resolve `{path}`: {e}"))?;
    if let Some(cwd) = working_dir {
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        if !canon.starts_with(cwd) {               // enforce containment
            return Err(format!("path `{path}` escapes the working directory"));
        }
    }
    Ok(canon)
}
```
*(For new-file writes where the target doesn't exist yet, canonicalize the parent and join the file name.)*

### #2 — reject command substitution in classification (C2)

**Before** — `core/src/tools/command_policy/mod.rs:113`
```rust
pub fn classify_command(command: &str) -> CommandSafety {
    shell_segments(peel_shell_wrapper(command))
        .into_iter()
        .map(|segment| classify_segment(&segment))
        .fold(CommandSafety::ReadOnly, |max, safety| max.max(safety))
}
```

**After**
```rust
pub fn classify_command(command: &str) -> CommandSafety {
    // shell_split can't parse these; the shell executes them, so any segment
    // containing unquoted command substitution must be treated as Danger.
    if has_unquoted_substitution(command) {
        return CommandSafety::Danger;
    }
    shell_segments(peel_shell_wrapper(command))
        .into_iter()
        .map(|segment| classify_segment(&segment))
        .fold(CommandSafety::ReadOnly, |max, safety| max.max(safety))
}

/// True if the command contains an unquoted `$(`, backtick, or `${` — none of
/// which `shell_split` understands, so the inner command is never classified.
fn has_unquoted_substitution(s: &str) -> bool {
    let mut single = false; let mut double = false; let mut esc = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => { esc = !esc; continue; }
            '\'' if !double => single = !single,
            '"'  if !single => double = !double,
            '$' if !esc && !single && chars.peek() == Some(&'(') => return true,
            '`' if !single && !double => return true,
            _ => {}
        }
        esc = false;
    }
    false
}
```

### #3 — don't delete config source on partial migration (config data-loss)

**Before** — `core/src/config/custom.rs:636` (loop) + `:694`
```rust
    for (namespace, kv) in &values {
        let page_path = dir.join(format!("{namespace}.yaml"));
        if !page_path.exists() { continue; }
        let Some(mut page) = load_yaml::<CustomConfigPage>(&page_path) else {
            continue;                                  // a corrupt page is silently skipped…
        };
        // …write page…
    }
    // …
    let _ = std::fs::remove_file(&values_path);        // …but the source is deleted anyway → data loss
}
```

**After**
```rust
    let mut migrated_all = true;
    for (namespace, kv) in &values {
        let page_path = dir.join(format!("{namespace}.yaml"));
        if !page_path.exists() { continue; }
        let Some(mut page) = load_yaml::<CustomConfigPage>(&page_path) else {
            eprintln!("bone: skipping migration of '{namespace}' (unparseable page); keeping config-values.yaml");
            migrated_all = false;                      // remember we couldn't migrate it
            continue;
        };
        // …write page…
    }
    // …status backfill…
    if migrated_all {                                  // only delete once everything landed
        let _ = std::fs::remove_file(&values_path);
    }
}
```

---

## Priority recommendation
Fix C1–C3 and the `setsid`/config-data-loss items first — they're surgical, net-low/negative LOC, and close real holes. Then make **approval an invariant inside `ToolRegistry::execute_live`** (the root cause behind C3 and several bypass paths) rather than a driver-level convention.

## Limitation / uncertainty
◑ items were reported by subagents and not all re-read line-by-line; the multi-statement exploitability of C4 depends on the (unread) `ctx.db.query` execute path, and should be confirmed before fixing. Everything tagged ✅ was read directly during this review.
