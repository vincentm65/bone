# Section 1 — Workspace architecture, protocol, entry points, distribution

**Review goal:** debloat, debug, simplify/streamline (not security theater).  
**Scope:** `protocol`, `core`, `tui` (excl. `webui/`). Version `2.4.1`, edition `2024`.  
**Mode:** investigation only.

Security notes appear only where they create **product complexity**, **misconfig footguns**, or **debug pain**. Default loopback `bone serve` without auth is a **local trust model**, not a release-blocker for a single-user desktop app.

---

## Architecture map (what actually ships)

```
bone binary (tui crate, ~928 LOC main.rs)
├── many CLI modes (see table)
├── in-process: Hub + run_daemon + TUI client (default)
└── optional TCP: bone serve / --connect / bone connect

bone-core
├── runtime::Driver     ← single agent loop (good)
├── agent::run_agent    ← setup wrapper + SessionWriter around Driver
├── run::run_headless   ← CLI parse → run_agent
├── rpc::{Hub, run_daemon, serve_*, SessionManager, RemoteClient}
├── RuntimeSession      ← durable conversation owner
└── re-exports protocol types in many modules

bone-protocol (~1.2k LOC) — wire SOOT; lean; not the bloat center
```

**Dependency direction:** OK (`tui → core → protocol`).  
**Intended story:** daemon authoritative, TUI is a client.  
**Actual story:** true for the event loop, but the **binary still hosts the daemon**, **re-exports all of core** (`tui/src/lib.rs:3`), and **main.rs owns install/deps/serve/web**.

---

## Entry surface inventory

| Mode | Purpose | Weight | Notes |
|------|---------|--------|-------|
| default TUI | interactive | necessary | boots in-process `run_daemon` |
| `--connect` | TUI → remote serve | medium | full client path |
| `serve` | multi-conversation TCP host | **heavy** | SessionManager + managed connections |
| `connect` | line-oriented debug client | small | useful; not in critical path |
| `run` | one-shot headless | medium | via `agent`/`Driver` |
| `web` | bridge launcher | small | webui out of scope |
| `setup` / `catalog` / `update` / `stats-popup` / `install` | product utilities | medium | ~deps+install alone ≈ 230 LOC in main |
| usage mentions `bone agent` | **stale** | — | **no such subcommand** |

`main.rs` rough composition:

| Block | ~LOC | Simplify angle |
|-------|------|----------------|
| deps auto-install | ~147 | extract or drop aggressive auto-install |
| `do_install` / PATH symlink | ~89 | extract `install.rs` |
| `run_serve` | ~160 | core of multi-client complexity |
| `run_connect` | ~57 | keep as debug tool or fold under `--connect --raw` |
| boot + default TUI | rest | keep |

---

## What is already streamlined (preserve)

1. **One agent loop:** `Driver` is shared; `run_agent` builds a Driver (`core/src/runtime/driver.rs` header, `agent.rs:544+`). Do not re-split headless vs TUI loops.
2. **Protocol crate is small and purposeful** — good boundary for non-Rust clients later.
3. **In-process TUI uses the same `run_daemon` command path as serve** — one control plane (flags differ: `inject_background`, `forward_view_diffs`).
4. **npm version `2.4.1` matches workspace**; platform wrapper is thin.

---

## Findings (debloat / debug / simplify)

### 1. Multi-client TCP stack is the largest architectural cost in this section
**Kind:** complexity / possible overbuild  
**Confidence:** verified

**Evidence:** production `bone serve` uses `SessionManager` + `serve_managed_connection` + per-conversation `run_daemon` actors (`tui/src/main.rs:335–468`, `core/src/rpc/mod.rs`).  
`serve_connection` (single-hub) remains for tests/integration only.

**Why it matters for the goal:** this is a second product (remote multi-chat host) living inside the local assistant. It forces:
- dual boot targets (`SessionTarget::{New,Latest,Conversation}`)
- remote vs local background-job injection flags
- `RemoteClient` + SocketConn + managed routing
- protocol commands that only exist to compensate for a VM-less remote frontend (`DispatchHook`, `SetTerminalWidth`, `AppendMessage`, …)

**Simplify options (pick one product intent):**
- **A. Local-first:** keep in-process daemon; treat `serve`/`--connect` as experimental; freeze protocol growth for remote-only knobs.
- **B. Daemon-first:** commit to serve as primary; then delete in-process special cases and make TUI always a socket client (even to a child process) — *one* path, more moving parts at runtime.
- **C. Status quo:** accept cost; document the two flags and two serve functions so contributors stop adding a third.

**Not required for debloat:** auth framework. If serve stays loopback-only and rare, a one-line trust warning beats a token system.

---

### 2. `main.rs` is a junk drawer (928 LOC)
**Kind:** bloat / navigation  
**Confidence:** verified

Mixes: panic guard, CLI parse, package-manager dependency install, symlink install, serve, connect, web, onboarding gate, default TUI boot.

**Smallest cleanup:**
- `cli.rs` — parse + usage (fix usage: remove phantom `bone agent`)
- `deps.rs` / `install.rs` — side utilities
- leave `main` as dispatch only

No behavior change; pure file split.

---

### 3. Approval mode parsed in ≥4 places
**Kind:** duplication → bugs  
**Confidence:** verified

| Site | Behavior |
|------|----------|
| `tui/src/main.rs:262` `approval_mode` | danger / else safe |
| `core/src/run.rs:219` `parse_approval` | safe\|read_only\|danger; **errors** on unknown |
| `core/src/rpc/mod.rs:590` `set_mode` | danger / else safe |
| `core/src/ext/ctx.rs` (~1106, ~2332) | safe\|read_only\|danger |

**Debug pain:** CLI `run --approval foo` fails; wire/UI/`set_mode` silently coerce to safe; Lua still accepts `read_only` alias.

**Fix:** one `ApprovalMode::parse(s) -> Result` (or `parse_lenient`) in `tools/approval.rs`; all call sites use it. Drop `read_only` or keep as single alias there only.

---

### 4. Two path authorities (`bone_dir` vs `db_path`)
**Kind:** bug + debug tax  
**Confidence:** verified

- Config/Lua/policy: `bone_dir()` → XDG or `~/.bone-rust` (`config/mod.rs:20-30`)
- DB: `dirs::home_dir()/.bone-rust/data/conversations.db` (`session_db.rs:11-16`)

**Symptom people hit:** “settings under XDG but empty/wrong history,” tests isolating only `XDG_CONFIG_HOME` still touch real home DB.

**Fix:** `db_path() = bone_dir().join("data/conversations.db")` + trivial migrate from legacy path. High leverage, low LOC.

Related: `/tmp/.bone-rust` when home missing (`config/mod.rs:27-30`) — fail closed or require `BONE_DIR` instead of silent shared temp (debug + multi-user footgun).

---

### 5. Persistence has three shapes
**Kind:** layering debt  
**Confidence:** verified

1. `SessionSink` trait + `SessionWriter` in `agent.rs` (headless / sub-agents)
2. `RuntimeSession` DB helpers (daemon transcript owner)
3. Direct `SessionDb` use from TUI stats / misc

`session_sink.rs` comments still narrate migration history (“Step 3…”, “future Driver”) — the Driver already exists. Docs/comments lag the architecture and slow readers.

**Simplify direction (later section 2/persistence):** one write path for “append message to conversation”; sinks become adapters, not parallel designs. Don’t invent a fourth.

---

### 6. Glob re-export `pub use bone_core::*` in the TUI crate
**Kind:** boundary blur  
**Confidence:** verified (`tui/src/lib.rs:3`)

Makes every UI module one `use bone::…` away from LLM/tools/Lua internals. Fights “TUI is a client” and hides what the binary API surface is.

**Fix:** stop glob; export `ui` + explicit facades only. Binary can `use bone_core` directly if needed.

---

### 7. Protocol command set grew for remote parity
**Kind:** streamline / YAGNI checkpoint  
**Confidence:** verified

`RuntimeCommand` includes remote-compensation ops: `SetApprovalMode`, `AppendMessage`, `ReplaceConversation`, `DispatchHook`, `SetTerminalWidth`, `KeymapDispatch`, `ReloadSettings`, … (`protocol/src/event.rs:187+`).

Each is justified *if* remote VM-less clients are first-class. If the product is local TUI + occasional `run`, this set is the main reason `rpc/mod.rs` is **1444 LOC**.

**Action:** annotate each command with owner path (`local TUI` / `serve` / `both`) in a short table; freeze remote-only additions until serve is committed product.

Protocol **types** themselves are fine — not bloated. Complexity is **command surface × daemon handler matrix**.

---

### 8. Stale CLI contract
**Kind:** dead docs  
**Confidence:** verified

Usage string advertises `bone agent` (`tui/src/main.rs:224`) but main dispatch has no `agent` subcommand. Real headless entry is `bone run`.

**Fix:** one-line usage edit. Same pass: list `setup|catalog|update|install|stats-popup` or stop claiming a short usage.

---

### 9. JSONL codec unbounded lines
**Kind:** reliability / debug  
**Confidence:** verified (`core/src/rpc/codec.rs`)

For local desktop this is mostly “don’t OOM if a client bugs out,” not a threat model item. A max line length is a **small reliability fix** if serve stays; ignore if serve is frozen experimental.

---

### 10. Packaging — keep as-is
npm optionalDeps + `bone.js` shim are minimal and version-aligned. Not a debloat target.

---

## Complexity budget (section 1)

| Subsystem | LOC signal | Verdict |
|-----------|------------|---------|
| `bone-protocol` | ~1.2k | Keep; lean |
| `runtime/driver` | ~1.2k | Keep; single loop |
| `agent` + `run` | ~0.9k | Keep wrapper; trim comments / merge approval parse |
| `rpc/mod.rs` | ~1.4k | **Prime simplify target** if serve scope shrinks |
| `tui/main.rs` | ~0.9k | **Split files**; delete stale usage |
| path helpers | tiny | **Unify** — high bugfix |

---

## Recommended streamline order (matches review goal)

1. **Unify `db_path` → `bone_dir()`** — correctness + debuggability, few LOC.
2. **One `ApprovalMode` parse** — delete 3 duplicates.
3. **Fix usage / drop `bone agent` myth**; optionally split `main.rs`.
4. **Decide serve product intent (A/B/C above)** before more protocol commands or auth work.
5. **Stop `pub use bone_core::*`** when touching TUI lib boundary.
6. **Only then** consider codec limits / serve trust warnings — polish, not architecture.

Defer deep `run_daemon` / SessionManager surgery to section 2 once serve intent is chosen.

---

## Contracts to preserve while simplifying

1. **Single agent loop = `Driver`.** Headless and interactive both build it; don’t fork loop bodies again.
2. **Daemon owns transcript + approval gate** for interactive modes (in-process or TCP).
3. **Protocol remains the only wire SOOT** — don’t move view/command types back into tui.
4. **Default approval is Safe**; only explicit danger elevates.
5. **One config root** after path fix — seeds, policy, DB, Lua all under `bone_dir()` (or documented `BONE_DIR`).
6. **JSONL = one object per line** — keep until a real need for another framer.
7. **Feature `tui` on core** stays optional for headless builds.

---

## Section exit checklist

| Goal lens | Status |
|-----------|--------|
| Map system boundaries | Done |
| Find duplication / dual paths | Done (#3–#5, dual serve) |
| Find dead/stale surface | Done (`bone agent`, migration comments) |
| Identify highest complexity per value | Done (multi-client RPC vs local Driver) |
| Security rabbit holes | Parked unless they block simplify |
| Concrete low-LOC wins | Path unify, approval parse, usage, main split |

---

## Cleanup status (implementation)

**Status: finished** (investigation findings applied + post-review fixes).

| Audit item | Done | Notes |
|------------|------|-------|
| #1 Serve product intent | document only | Status quo (local-first + optional TCP); ownership table on `RuntimeCommand` |
| #2 Split `main.rs` | yes | `tui/src/{cli,deps,install}.rs`; main is dispatch |
| #3 One `ApprovalMode` parse | yes | `parse` / `parse_lenient` in `tools/mod.rs`; all call sites |
| #4 Unify `db_path` → `bone_dir` | yes | + legacy hard-link/copy after WAL checkpoint |
| #5 Persistence shapes | docs only | `session_sink` comments updated; no third write path |
| #6 Stop glob re-export | yes | Explicit `pub use bone_core::{…}` in `tui/src/lib.rs` |
| #7 Protocol command ownership | yes | Table on `RuntimeCommand` |
| #8 Stale `bone agent` usage | yes | `cli.rs` usage lists real modes |
| #9 JSONL max line | yes | `MAX_LINE_BYTES` + `ReadError::TooLong` |
| #10 Packaging | keep | unchanged |

**Post-review fixes:**
- `try_bone_dir()` so `ensure_deps` does not panic before `--help` in stripped envs
- Codex auth path uses `$HOME/.codex`, not `bone_dir().parent()`
- Legacy DB migrate checkpoints WAL (+ copy `-wal`/`-shm` on copy fallback)
- `DaemonCtx::set_mode` uses strict `parse` and reports unknown values
- Wire orphaned `custom_tests.rs` via `#[path]` under `custom.rs`
- Oversized-line codec test uses a larger duplex buffer

**Verify:** `cargo fmt --check`; `cargo check -p bone-core -p bone --tests`; unit tests for `config::`, `session_db`, `rpc::codec`, `custom_tests`, approval parse, TUI render.

---

## Evidence index

| Topic | Location |
|-------|----------|
| Workspace | `Cargo.toml` |
| Protocol commands/events | `protocol/src/event.rs` |
| Codec | `core/src/rpc/codec.rs` |
| Driver as single loop | `core/src/runtime/driver.rs:1-6`, `agent.rs:544+` |
| RPC surface | `core/src/rpc/mod.rs` |
| Path root | `config/mod.rs` (`bone_dir` / `try_bone_dir`), `session_db.rs` (`db_path`) |
| Approval parsers | `tools/mod.rs` (`ApprovalMode::parse`), call sites in `run`, `rpc`, `ext/ctx`, `cli` |
| CLI / usage | `tui/src/cli.rs` |
| Core re-export | `tui/src/lib.rs` |
| npm | `npm/bone-agent/package.json` |
