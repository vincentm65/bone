# Plan: TUI / Runtime Decoupling — Phase 1 (core data types)

Status: design. Last updated against the state after Steps 0–3 (provider /
approval / extensions / session seams).

## 0. Goal

Make bone's core (agent loop, tools, extensions, llm, session) compilable and
usable **without** the `ui/` module and **without** ratatui. The built-in TUI
remains the primary frontend and keeps ratatui; it just stops being a
dependency of the core. A second frontend (web, different TUI) should be
buildable against the core alone.

This is the Neovim model: core defines abstract data, frontends render it,
ratatui lives only in the built-in TUI.

## 1. Why now, and what's already done

Steps 0–3 made the agent loop's four runtime dependencies injectable
(provider, approval policy, extensions, session sink). Those seams are
necessary but sit on top of a data model that still assumes a terminal:

```
src/tools/types.rs:1   use crate::ui::pane_page::PanePage;     ← core imports ui
src/ext/ctx.rs:16      use crate::ui::pane_page::{...};         ← core imports ui
src/ext/lua_tool.rs:15 use crate::ui::pane_page::PanePage;      ← core imports ui
```

`PanePage.content` is `Vec<ratatui::text::Line<'static>>` — a terminal
rendering type baked into `ToolResult`, the core type that flows through the
entire agent loop. This is the root cause of the backwards dependency. It must
be fixed before the loop can be extracted (Phase 2), because the loop body
touches `ToolResult` on every iteration.

The dependency direction today: **core → ui → ratatui** (backwards).
The goal: **ui → core** (correct); ratatui stays in ui.

## 2. What moves

The single root cause is `PanePage` living in `ui/` while being used by core.
Its content field carries `ratatui::text::Line`, which drags ratatui into core.

The fix is to split `PanePage` into two layers:

- **`PaneContent`** (new, core) — a plain, serde-ready data type. `lines` is
  `Vec<PaneLineSpec>` where each line is either a plain `String` or a list of
  `{text, fg?, modifiers?}` span specs. No ratatui.
- **`PanePage`** (stays in `ui/`) — keeps `content: Vec<ratatui::text::Line>`.
  Gains a `from_content(PaneContent) -> PanePage` constructor that does the
  ratatui conversion. All TUI rendering code is unchanged.

Then core types reference `PaneContent` instead of `PanePage`, and the
conversion happens at the ui boundary.

### 2.1 `PaneContent` — the new core type

Lives in a new `src/pane_content.rs` module (core, not ui).

```rust
/// Plain, serializable pane content. Frontend-agnostic.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default, PartialEq)]
pub struct PaneContent {
    pub source: String,
    pub title: String,
    pub lines: Vec<PaneLineSpec>,
    pub visible_rows: usize,
    pub scroll: usize,
}

/// One line of pane content: either plain text or styled spans.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PaneLineSpec {
    Plain(String),
    Spans { spans: Vec<PaneSpanSpec> },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PaneSpanSpec {
    pub text: String,
    pub fg: Option<String>,
    pub modifiers: Option<Vec<PaneModifier>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum PaneModifier { Bold, Dim, Italic, Strike }
```

This mirrors exactly what Lua already passes to `PanePage::from_json` today
(verified: `pane_page.rs:396` accepts plain strings and `{text, fg, modifiers}`
span objects). `PaneContent` is what that JSON *is*; today it's eagerly
converted to ratatui and stored. The change is: store it as data, convert to
ratatui later, at the boundary.

### 2.2 `PaneInteraction` / `InteractionMode`

These also leak into core via `ext/ctx.rs:16`. They are TUI interaction state
(cursor position, selected index, input buffer, key handling). Inspected at
`pane_page.rs:8-35`: `InteractionMode` is a pure enum (`SingleSelect`,
`MultiSelect`, `TextInput`) and `PaneInteraction` wraps an
`Arc<Mutex<InteractionInner>>` with a `oneshot::Sender` for results.

Decision: **move `InteractionMode` into core** as `PaneInteractionMode` (it's
just an enum, no ratatui). Leave `PaneInteraction` (the mutable stateful
handle with key-handling logic) in `ui/` — it's inherently terminal-input
state and does not belong in core. Core carries the *mode* (so Lua can request
it); the TUI owns the *live interaction state*.

`InteractionMode` moves to `src/pane_content.rs` as `PaneInteractionMode` and
gets serde derives. The TUI re-exports it under the old name or adapts.

## 3. Steps

### Step 4 — Create `PaneContent` in core

**Files:**
- `src/pane_content.rs` (new): `PaneContent`, `PaneLineSpec`, `PaneSpanSpec`,
  `PaneModifier`, `PaneInteractionMode`. All derive `Serialize`/`Deserialize`.
- `src/lib.rs`: add `pub mod pane_content`.

**Acceptance:**
- New test `tests/pane_content_test.rs`: round-trip serde, construct from the
  same JSON shapes that Lua passes today (plain-string lines, span lines,
  mixed), `Default` produces empty.
- `cargo build` succeeds (no usages yet, just the type).

### Step 5 — Add `PanePage::from_content(PaneContent)` in ui

**Files:**
- `src/ui/pane_page.rs`: add `from_content(&PaneContent) -> PanePage` that
  converts each `PaneLineSpec` → `ratatui::text::Line` (same logic currently
  inline in `from_json`, lines 396–470, just relocated). The existing
  `from_json` stays for now (used by Lua); it becomes a thin wrapper:
  `serde_json::from_value` into `PaneContent`, then `from_content`.

**Acceptance:**
- Existing `from_json` behavior unchanged (test: feed the same JSON, assert
  identical `PanePage` output).
- `from_content` produces the same ratatui structure that `from_json` does
  today (conversion test).

### Step 6 — Swap `PanePage` → `PaneContent` in core types

This is the step that fixes the dependency direction. It touches:

**Files:**
- `src/tools/types.rs:1,29,55,83`: replace `use crate::ui::pane_page::PanePage`
  with `use crate::pane_content::PaneContent`. Change `ToolResult.pane_page:
  Option<PanePage>` → `pane_content: Option<PaneContent>`. Change
  `ToolLiveEvent::Pane(PanePage)` → `ToolLiveEvent::Pane(PaneContent)`.
- `src/ext/lua_tool.rs:15,302-307`: build `PaneContent` from the Lua JSON
  directly (via `serde_json::from_value`) instead of calling
  `PanePage::from_json`. No ui import.
- `src/ext/ctx.rs:16`: drop the ui import. The `ctx.pane()` function builds a
  `PaneContent` and stores it. `InteractionMode` → `PaneInteractionMode` from
  core. (The `ctx.interact()` path references `PaneInteraction` — that stays a
  TUI concern; see Step 6.1.)

**Step 6.1 — the `ctx.interact()` problem.** `ctx.rs:477` creates a
`PaneInteraction` (TUI stateful handle) inside the Lua API. This is the
hardest coupling point. The interaction needs a live TUI to collect keypresses
and a `oneshot` channel to return the result. For the core to be usable
headless, this must degrade gracefully when no TUI is attached. Options:
  - (a) `ctx.interact()` returns an error/nil when no frontend interaction
    handler is registered (simplest, keeps interaction TUI-only for now).
  - (b) Extract an `InteractionHandler` trait that the TUI implements; core
    holds `Option<Arc<dyn InteractionHandler>>`.

Recommendation: **(a) for Phase 1.** Interaction is a terminal concept today;
making it frontend-portable is a later phase. The `ctx.interact()` Lua function
checks a registered handler and returns `(false, "interaction unavailable")`
when none — which is already the headless fallback at `ctx.rs:465`.

**Files affected in ui (adapt to renamed core field):**
- `src/ui/app/stream/mod.rs:742,764`: `ToolLiveEvent::Pane(content)` now
  carries `PaneContent`; convert to `PanePage` via `from_content` at the
  boundary before rendering.
- `src/ui/app/mod.rs`: any `pane_page` field reads → `pane_content`.
- `src/ui/render/bottom_pane.rs`, `src/ui/render/mod.rs`,
  `src/ui/subagent_pane.rs`: adapt field names.
- `tests/bottom_pane_test.rs`, `tests/stream_tools_test.rs`: update
  construction/assertions to use `PaneContent` or `PanePage::from_content`.

**Acceptance:**
- `grep -rn 'use crate::ui' src/tools/ src/ext/ src/agent.rs src/run.rs
  src/llm/ src/session_db.rs src/session_sink.rs` returns **zero matches**.
- Core compiles with ratatui removed from `Cargo.toml` `[dependencies]`
  (verified via a temporary `cargo check` — see Step 7).
- Full test suite green.
- Lua `ctx.pane()` still works (integration test or existing lua_api_test).

### Step 7 — Verify core is ratatui-free (gate, don't ship)

Add a CI-style check to prove the dependency is gone:

**Action:** temporarily comment out ratatui from `Cargo.toml`, run
`cargo check --lib` (core only, no ui), confirm it compiles, then revert.

This is a verification step, not a permanent change — the TUI still needs
ratatui. But it proves core is standalone. If desired, a permanent gate can be
added later via a `core` cargo feature that excludes `ui`.

**Acceptance:**
- `cargo check --lib` succeeds with ratatui commented out.
- `cargo test` (full, with ratatui restored) is green.

## 4. Risk analysis

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Renaming `pane_page` field across ui breaks subtle render logic | Medium | Step 5 ensures conversion happens at one boundary (`from_content`); all render code keeps using `PanePage` with `Vec<Line>` unchanged. |
| `ctx.interact()` headless degradation changes Lua behavior | Low | Already returns `(false, "interaction unavailable")` when no handler (`ctx.rs:465`); Step 6.1 just makes that the explicit headless path. |
| `PaneContent` JSON shape diverges from what `from_json` accepts today | Low | Step 5 makes `from_json` delegate through `PaneContent`; the wire format is the same JSON that Lua already produces. |
| Core `cargo check` without ratatui reveals more transitive leaks | Medium | Step 7 is the discovery mechanism. If new leaks surface, they are additional `use crate::ui` violations to fix (same pattern). |

## 5. What is explicitly NOT in this phase

- **Loop extraction** (the `run_agent` / TUI loop body unification) — Phase 2.
  Blocked on this phase (the loop touches `ToolResult` every iteration).
- **Async approval trait** — Phase 2. The approval *logic* is already pure
  (Step 1); the async *mechanism* needs the loop extracted first.
- **Protocol / daemon / transport** — Phase 3. Needs the loop extracted and
  events serializable.
- **Rewriting the TUI as a client** — Phase 4. Needs the protocol.
- **`ctx.interact()` as a portable trait** — later. Interaction stays TUI-only
  with graceful headless degradation for now.

## 6. Prerequisites for Phase 2 (what this phase unlocks)

After Phase 1:
- Core has zero ui dependency → a `Driver` struct can live in core and be
  unit-tested without a terminal.
- `ToolResult` carries `PaneContent` (serde) → events are wire-ready.
- `ToolLiveEvent::Pane(PaneContent)` (serde) → live pane updates can go over a
  protocol.
- The loop body can be extracted knowing its types are all frontend-agnostic.

---

## 7. Implementation appendix: complete type definitions + edit ordering

This appendix fills the gaps in Steps 4–7: exact type definitions, the
`interact()` redesign, field-by-field consumer mapping, and a compilable edit
ordering verified against the current codebase.

### 7.1 Type definitions — `src/pane_content.rs` (new file, core)

All types are pure data with serde derives. No ratatui, no `crate::ui`.

```rust
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// What kind of interaction the user is performing.
/// Moved from `ui::pane_page` — core owns this because core constructs it
/// from Lua opts before sending it to the frontend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionMode {
    SingleSelect,
    MultiSelect,
    TextInput,
}

/// One span within a styled line. `fg` is a color name string (parsed by the
/// frontend at render time via `ext::color::parse_color`). `modifiers` is a
/// list of strings ("bold", "dim", "italic", "strike"/"crossed_out"); unknown
/// values are silently ignored (same as today).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSpanSpec {
    pub text: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

/// One line of pane content. Either a plain string or a list of styled spans.
/// `#[serde(untagged)]` gives us the dual format that `from_json` parses today
/// (line element is either `"text"` or `{"spans": [...]}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaneLineSpec {
    Plain(String),
    Spans { spans: Vec<PaneSpanSpec> },
}

impl PaneLineSpec {
    /// True if the line renders no visible text.
    pub fn is_empty(&self) -> bool {
        match self {
            PaneLineSpec::Plain(s) => s.is_empty(),
            PaneLineSpec::Spans { spans } => spans.is_empty(),
        }
    }
}

/// Pure-data representation of a pane page. This is what flows through core
/// types (`ToolResult`, `ToolLiveEvent`). The TUI converts it to its internal
/// `PanePage` (with `Vec<ratatui::text::Line>`) via `PanePage::from_content`.
///
/// Replaces `PanePage` in all core type definitions. `lines` replaces
/// `PanePage.content: Vec<Line>`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneContent {
    pub source: String,
    pub title: String,
    #[serde(default)]
    pub lines: Vec<PaneLineSpec>,
    #[serde(default = "default_visible_rows")]
    pub visible_rows: usize,
    #[serde(default)]
    pub scroll: usize,
}

fn default_visible_rows() -> usize { 8 }

impl PaneContent {
    /// True when this content signals pane removal (empty lines = remove).
    /// Replaces the `page.content.is_empty()` checks scattered across core.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Parse from the JSON value that Lua produces (same format `from_json`
    /// accepted before). Delegates to serde deserialization.
    pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<PaneContent>(val.clone())
            .map_err(|e| format!("pane parse error: {e}"))
    }
}

/// A request for user interaction (single-select, multi-select, or text
/// input). Sent as a `ToolLiveEvent::Interact`. The `reply` channel carries
/// the user's response back to the blocked Lua caller.
///
/// This type is NOT `Clone` and NOT serializable — it carries a live oneshot
/// sender. For wire protocols (Phase 3), the serializable portion is the
/// fields minus `reply`; the protocol layer reconstructs the channel.
pub struct InteractRequest {
    pub question: String,
    pub mode: InteractionMode,
    pub options: Vec<String>,
    pub default_selected: usize,
    pub allow_custom: bool,
    pub reply: oneshot::Sender<serde_json::Value>,
}
```

**Design notes:**

- No `PaneModifier` enum. The current code silently ignores unknown modifier
  strings (`_ => {}` in `from_json`). Keeping `modifiers: Vec<String>` and
  doing the match at the TUI boundary (`from_content`) preserves this behavior.
  A serde enum would reject unknown variants.

- `InteractionMode` gets `PartialEq, Eq` added (current derive is just
  `Clone, Debug`). All uses are `matches!()` comparisons, so `PartialEq` is
  unused today but costs nothing and may help tests.

- `PaneContent::is_empty()` centralizes the "remove pane" signal. Today this
  check (`page.content.is_empty()`) appears in 4 places; after the swap all
  call `content.is_empty()` which delegates to `lines.is_empty()`.

### 7.2 `ToolLiveEvent` redesign

Current:
```rust
#[derive(Debug, Clone)]
pub enum ToolLiveEvent {
    Pane(PanePage),
}
```

New:
```rust
#[derive(Debug)]
pub enum ToolLiveEvent {
    /// Upsert a pane (or remove when `PaneContent::is_empty()`).
    Pane(PaneContent),
    /// Request user interaction; block until `reply` resolves.
    Interact(InteractRequest),
}
```

**`Clone` is dropped.** `InteractRequest` contains a `oneshot::Sender` which
is not `Clone`. This is safe: verified that `ToolLiveEvent` is never cloned
anywhere. The only `.clone()` in the consuming code is `page.source.clone()`
(`stream/mod.rs:768`), which clones the `String`, not the event.

### 7.3 The `interact()` redesign — core side (`ctx.rs`)

The current `ctx.ui.interact()` body (`ctx.rs:470–560`) does three things:
1. Parse opts from Lua → mode, options, default, allow_custom.
2. Build ratatui `Line`/`Span`/`Style` for the question text.
3. Construct `PaneInteraction::new(...)` + `PanePage { content: lines, ... }`,
   send `ToolLiveEvent::Pane(page)`, block on the reply.

Steps 2–3 move to the TUI. Core does only step 1 + sends an
`InteractRequest`:

```rust
// ctx.rs — inside the interact_fn closure (replaces lines 483–560)
let mode = match type_str.as_str() { /* same match, unchanged */ };
// validation — same as today (lines 492–497)

let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
let request = crate::pane_content::InteractRequest {
    question,
    mode,
    options,
    default_selected: default.map(|d| d.saturating_sub(1)).unwrap_or(0),
    allow_custom: allow_custom || !matches!(mode, InteractionMode::TextInput),
    reply: tx,
};

let _lock = INTERACT_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
sender
    .send(crate::tools::types::ToolLiveEvent::Interact(request))
    .map_err(|e| mlua::Error::external(format!("interact send failed: {e}")))?;

// Block until user responds (lock held, serializing concurrent calls).
let result: serde_json::Value = tokio::task::block_in_place(|| {
    tokio::runtime::Handle::current().block_on(rx)
}).map_err(|e| mlua::Error::external(format!("interact cancelled: {e}")))?;

let lua_result = lua.to_value(&result)
    .map_err(|e| mlua::Error::external(format!("interact result conversion: {e}")))?;
Ok(lua_result)
```

What disappears from `ctx.rs`: the ratatui `Line`/`Span::styled`/`Style`
construction (lines 500–506), the `visible_rows` computation (lines
512–519), and the `PaneInteraction::new` + `PanePage { ... }` construction
(lines 521–546). All of this moves to `PanePage::from_interact` in the TUI.

**Import change in `ctx.rs`:**
```
// Before:
use crate::ui::pane_page::{InteractionMode, PaneInteraction, PanePage};
// After:
use crate::pane_content::InteractionMode;
```
`PaneInteraction` and `PanePage` are no longer named in `ctx.rs` at all.

### 7.4 Boundary conversion methods — TUI side (`pane_page.rs`)

Two new methods on the TUI's `PanePage`. The conversion logic is extracted
verbatim from the existing code — same output, just called at a different
point.

```rust
impl PanePage {
    /// Convert pure-data `PaneContent` into a renderable `PanePage`.
    /// This is the ratatui conversion that used to live inline in `from_json`
    /// (lines 396–496). Same logic, same output.
    pub fn from_content(content: &crate::pane_content::PaneContent) -> Self {
        let lines: Vec<Line<'static>> = content.lines.iter()
            .map(|spec| {
                use crate::pane_content::PaneLineSpec;
                match spec {
                    PaneLineSpec::Plain(text) => Line::from(text.clone()),
                    PaneLineSpec::Spans { spans } => {
                        let ratatui_spans: Vec<Span<'static>> = spans.iter()
                            .map(|s| {
                                let mut style = Style::default();
                                if let Some(fg) = &s.fg
                                    && let Some(c) = crate::ext::color::parse_color(fg)
                                {
                                    style = style.fg(c);
                                }
                                for m in &s.modifiers {
                                    match m.as_str() {
                                        "bold" => style = style.add_modifier(Modifier::BOLD),
                                        "dim" => style = style.add_modifier(Modifier::DIM),
                                        "italic" => style = style.add_modifier(Modifier::ITALIC),
                                        "strike" | "crossed_out" => {
                                            style = style.add_modifier(Modifier::CROSSED_OUT);
                                        }
                                        _ => {}
                                    }
                                }
                                Span::styled(s.text.clone(), style)
                            })
                            .collect();
                        if ratatui_spans.is_empty() {
                            Line::from("")
                        } else {
                            Line::from(ratatui_spans)
                        }
                    }
                }
            })
            .collect();
        PanePage {
            source: content.source.clone(),
            title: content.title.clone(),
            content: lines,
            visible_rows: content.visible_rows,
            scroll: content.scroll,
            interaction: None,
        }
    }

    /// Build an interactive pane page from an `InteractRequest`.
    /// Extracted from the old `ctx.rs` interact body (lines 500–546).
    /// Builds question text Lines, computes visible_rows, and creates
    /// the `PaneInteraction` that owns the reply channel.
    pub fn from_interact(req: crate::pane_content::InteractRequest) -> Self {
        use crate::pane_content::InteractionMode;
        let question = req.question;
        let allow_custom = req.allow_custom;

        // Build question content lines (moved verbatim from ctx.rs:500–506)
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            question.clone(),
            Style::default().fg(ratatui::style::Color::White),
        )));
        lines.push(Line::from(""));

        // Compute visible_rows (moved verbatim from ctx.rs:511–519)
        const MAX_VISIBLE_OPTIONS: usize = 10;
        let opt_rows = if matches!(req.mode, InteractionMode::TextInput) {
            1
        } else {
            req.options.len().min(MAX_VISIBLE_OPTIONS)
        };
        let custom_row = u16::from(allow_custom);
        let visible_rows = (lines.len() + opt_rows + custom_row as usize)
            .min(24).max(3);

        let interaction = PaneInteraction::new(
            req.mode,
            req.options,
            allow_custom,
            req.default_selected,
            req.reply,
        );

        PanePage {
            source: "interact".to_string(),
            title: format!("Question — {:?}", req.mode),
            content: lines,
            visible_rows,
            scroll: 0,
            interaction: Some(interaction),
        }
    }
}
```

**`from_json` becomes a thin delegate:**
```rust
pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
    let content = crate::pane_content::PaneContent::from_json(val)?;
    Ok(Self::from_content(&content))
}
```

**`InteractionMode` re-export** stays in `pane_page.rs` for TUI-internal
callers that import from there:
```rust
pub use crate::pane_content::InteractionMode;
```
This keeps `ui/render/bottom_pane.rs`'s `use crate::ui::pane_page::{InteractionMode, PaneInteraction}` working unchanged.

### 7.5 Consumer mapping — every site that changes

#### Core types (`src/tools/types.rs`)

| Line | Before | After |
|------|--------|-------|
| 1 | `use crate::ui::pane_page::PanePage;` | `use crate::pane_content::PaneContent;` |
| 29 | `pub pane_page: Option<PanePage>,` | `pub pane_page: Option<PaneContent>,` |
| 55 | `pub pane_page: Option<PanePage>,` | `pub pane_page: Option<PaneContent>,` |
| 80–84 | `#[derive(Debug, Clone)] enum ToolLiveEvent { Pane(PanePage) }` | `use crate::pane_content::{PaneContent, InteractRequest}; #[derive(Debug)] enum ToolLiveEvent { Pane(PaneContent), Interact(InteractRequest) }` |

Field name stays `pane_page` — only the type changes. This avoids renaming
across 15+ construction sites that set `pane_page: None`.

#### Core reads (`src/agent.rs`)

| Line | Before | After |
|------|--------|-------|
| 740 | `.map(\|p\| p.source.as_str())` | `.map(\|p\| p.source.as_str())` — **unchanged** (`PaneContent` also has `source`) |
| 742 | `page.content.is_empty()` | `page.is_empty()` (or `page.lines.is_empty()`) |

#### Core reads (`src/tools/registry.rs`)

| Line | Before | After |
|------|--------|-------|
| 72 | `pane_page: output.pane_page,` | **unchanged** — type flows through |
| 132 | `page.content.is_empty()` | `page.is_empty()` |

#### Core reads (`src/ext/lua_tool.rs`)

| Line | Before | After |
|------|--------|-------|
| 15 | `use crate::ui::pane_page::PanePage;` | `use crate::pane_content::PaneContent;` |
| 304 | `PanePage::from_json(pane_val).ok()` | `PaneContent::from_json(pane_val).ok()` |

This is the only core site that calls `from_json` on a pane. After the swap
it calls `PaneContent::from_json` (core method, no ratatui).

#### Core writes (`src/ext/ctx.rs`)

| Line | Before | After |
|------|--------|-------|
| 16 | `use crate::ui::pane_page::{InteractionMode, PaneInteraction, PanePage};` | `use crate::pane_content::InteractionMode;` |
| 456 | `PanePage::from_json(&val)` | `PaneContent::from_json(&val)` |
| 458 | `ToolLiveEvent::Pane(pane)` | **unchanged** (now takes `PaneContent`, which is what `from_json` returns) |
| 470–560 | interact body (ratatui construction) | replaced by `InteractRequest` + `ToolLiveEvent::Interact` (see 7.3) |
| 1093 | `PanePage::from_json(&val)` | `PaneContent::from_json(&val)` |
| 1095 | `ToolLiveEvent::Pane(pane)` | **unchanged** |

#### TUI consumer (`src/ui/app/stream/mod.rs`)

This file receives `ToolResult.pane_page` and `ToolLiveEvent` from core.
All `PanePage` references here are about the TUI's internal storage
(`self.pages: Vec<PanePage>`), which stays as-is. Only the boundary
conversions are new.

| Line | Before | After |
|------|--------|-------|
| 12 | `use crate::ui::pane_page::PanePage;` | **unchanged** (TUI still uses PanePage internally) |
| 583–597 | reads `result.pane_page` as `PanePage`, checks `page.content.is_empty()`, calls `PanePage::upsert(&mut pages, ..., page.clone())` | convert first: `if let Some(pc) = &result.pane_page { let page = PanePage::from_content(pc); ... }` — check `pc.is_empty()` for the remove-vs-upsert decision, then `PanePage::upsert` receives the converted `PanePage` |
| 634 | `pane_page: None,` | **unchanged** |
| 742 | `ToolLiveEvent::Pane(page)` → `page.content.is_empty()` | `ToolLiveEvent::Pane(pc)` → `PanePage::from_content(pc)` then proceed as before; **add** `ToolLiveEvent::Interact(req)` arm → `PanePage::from_interact(req)` then `PanePage::upsert` |
| 764 | `let ToolLiveEvent::Pane(page) = &event;` | must handle both variants: `match &event { ToolLiveEvent::Pane(pc) => ..., ToolLiveEvent::Interact(req) => ... }` |
| 920 | `pane_page: None,` | **unchanged** |

#### Other TUI files — **no changes**

These files work exclusively with the TUI's `PanePage` (ratatui type) and
never touch core types:

- `src/ui/pane_page.rs` — adds `from_content`/`from_interact`, keeps everything else
- `src/ui/app/mod.rs` — imports `PanePage` from itself; unchanged
- `src/ui/render/bottom_pane.rs` — renders `PanePage`; unchanged
- `src/ui/render/mod.rs` — renders `PanePage`; unchanged
- `src/ui/subagent_pane.rs` — builds `PanePage` directly (not from core); unchanged

### 7.6 Compilable edit ordering — Step 6

The constraint: the crate must compile after each sub-step. The key insight is
that `PaneContent` and `PanePage` can coexist — the swap is replacing where
each is *used in core types*, not eliminating either type. Steps 4–5 add
`PaneContent` and the boundary methods while `PanePage` is still in core;
Step 6 does the swap; Step 7 proves it.

#### Step 4 — Create `src/pane_content.rs` (additive, no existing code touched)

1. Write `src/pane_content.rs` with all types from §7.1.
2. Add `pub mod pane_content;` to `lib.rs`.

**Verify:** `cargo check --lib` — compiles (new module, nothing uses it yet).

#### Step 5 — Move `InteractionMode` to core + add boundary methods

5a. Move `InteractionMode`:
1. Delete `pub enum InteractionMode { ... }` from `pane_page.rs`.
2. Add `pub use crate::pane_content::InteractionMode;` to `pane_page.rs`.
3. Change `ctx.rs` line 16: `use crate::pane_content::InteractionMode;`
   (remove `PaneInteraction, PanePage` from this import — they're still used
   in the interact body, so add a separate `use crate::ui::pane_page::{PaneInteraction, PanePage};` line temporarily; it gets deleted in 6.2).

**Verify:** `cargo check` — compiles. All TUI callers that wrote
`use crate::ui::pane_page::InteractionMode` still work via the re-export.

5b. Add boundary methods:
1. Add `PanePage::from_content()` to `pane_page.rs` (§7.4).
2. Add `PanePage::from_interact()` to `pane_page.rs` (§7.4).
3. Change `from_json` to delegate through `PaneContent::from_json` +
   `from_content` (§7.4).

**Verify:** `cargo test` — green. `from_json` now produces identical output
through the new path.

#### Step 6 — Swap core types (the atomic step)

This must be done as a single coordinated edit across the 4 core files, then
the 1 TUI file. The crate will not compile between sub-edits — that's
expected. Make all edits, then compile.

6.1. `src/tools/types.rs`:
- Import: `use crate::pane_content::{PaneContent, InteractRequest};`
- `ToolResult.pane_page` → `Option<PaneContent>`
- `ToolOutput.pane_page` → `Option<PaneContent>`
- `ToolLiveEvent` → drop `Clone`, add `Interact` variant (§7.2)

6.2. `src/ext/ctx.rs`:
- Remove temporary `use crate::ui::pane_page::{PaneInteraction, PanePage};`
- `from_json` calls → `PaneContent::from_json` (lines 456, 1093)
- Replace interact body with `InteractRequest` + `Interact` variant (§7.3)

6.3. `src/ext/lua_tool.rs`:
- Import: `use crate::pane_content::PaneContent;`
- Line 304: `PaneContent::from_json(pane_val).ok()`

6.4. `src/agent.rs`:
- Line 742: `page.content.is_empty()` → `page.is_empty()`

6.5. `src/tools/registry.rs`:
- Line 132: `page.content.is_empty()` → `page.is_empty()`

6.6. `src/ui/app/stream/mod.rs`:
- `apply_tool_live_event`: add `Interact` arm, convert `Pane` via
  `from_content` (§7.5).
- `apply_and_track`: convert `let ToolLiveEvent::Pane(page) = &event;` to a
  `match` handling both variants.
- Tool-result pane processing (lines 583–597): convert `result.pane_page`
  via `from_content` before calling `upsert`/`remove`.

**Verify:** `cargo check` then `cargo test` — all green. **At this point no
core file has `use crate::ui` — verify with:**
```bash
grep -rn 'use crate::ui' src/tools/ src/ext/ src/agent.rs src/run.rs src/session_sink.rs
# Expected: no output
```

#### Step 7 — Gate: compile core without ratatui

1. Temporarily comment out `pub mod ui;` and ratatui/crossterm deps in
   `Cargo.toml`.
2. `cargo check --lib` — must compile.
3. Restore `Cargo.toml` and `pub mod ui;`.
4. `cargo test` — full suite green.

If new leaks surface, they are additional `use crate::ui` violations in core
to fix (same pattern: extract a pure-data type, convert at the boundary).

### 7.7 Summary of what moves where

```
BEFORE                                  AFTER
─────────────────────────────────────────────────────────────────
ui::pane_page::InteractionMode    ──►   pane_content::InteractionMode
ui::pane_page::PanePage           ──►   pane_content::PaneContent  (in core types)
                                      + ui::pane_page::PanePage     (in TUI only)

tools/types.rs:
  ToolResult.pane_page: PanePage  ──►   ToolResult.pane_page: PaneContent
  ToolOutput.pane_page: PanePage  ──►   ToolOutput.pane_page: PaneContent
  ToolLiveEvent::Pane(PanePage)   ──►   ToolLiveEvent::Pane(PaneContent)
                                   +   ToolLiveEvent::Interact(InteractRequest)
  #[derive(Clone)]                ──►   (removed)

ctx.rs interact body:
  builds ratatui Lines            ──►   sends InteractRequest
  builds PaneInteraction          ──►   TUI builds it in from_interact
  sends ToolLiveEvent::Pane       ──►   sends ToolLiveEvent::Interact

pane_page.rs:
  from_json (JSON→ratatui inline) ──►   from_content (PaneContent→ratatui)
                                   +   from_interact (InteractRequest→PanePage)
                                   +   from_json delegates through PaneContent
```
