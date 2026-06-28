# Modular Theme System Implementation Plan

**Status:** Proposed  
**Scope:** `protocol`, `core`, and `tui` theme boundaries and rendering  
**Primary constraint:** Theme semantics and Ratatui types must remain outside
`bone-core`. Core may transport theme data from Lua, but it must not enumerate
or interpret TUI theme keys.

## 1. Outcome

Build one app-wide theme system that:

- styles the transcript, Markdown, syntax highlighting, bottom panes, jobs,
  setup, catalog, pickers, and stats;
- supports built-in dark, light, and ANSI-safe presets;
- preserves the existing `bone.theme` flat-key configuration;
- supports structured palette and component-style overrides;
- keeps Lua parsing, wire transport, theme resolution, and Ratatui rendering in
  separate layers;
- restores the configured theme after temporary runtime highlights are cleared;
- remains compatible with the planned removal of the TUI's local Lua VM.

This is a staged refactor. It does not require replacing the renderer, changing
the terminal backend, or completing the daemon-only Lua migration.

## 2. Non-negotiable module boundaries

The dependency direction must be:

```text
init.lua
   |
   | generic Lua-table conversion
   v
bone-core/ext  ------>  bone-protocol::ThemeDocument
                              |
                              | opaque serialized data
                              v
                     tui::ui::theme resolver
                              |
                              | resolved Ratatui styles
                              v
                     TUI rendering components
```

The following rules are acceptance criteria, not preferences:

1. `bone-protocol` contains serializable data only. It must not depend on
   `mlua`, `ratatui`, `crossterm`, or `syntect`.
2. `bone-core` may read `bone.theme` from Lua, but its Rust implementation must
   not contain semantic keys such as `user_msg`, `diff_added`,
   `markdown.heading`, or `accent`. Bundled user documentation may list the
   public configuration API.
3. `bone-core` must not parse colors, choose presets, calculate contrast, or
   create render styles.
4. `tui` exclusively owns color parsing, presets, fallback rules, semantic
   aliases, validation, syntax-theme selection, and Ratatui `Style` values.
5. Rendering modules receive a resolved theme explicitly. They do not read
   Lua, extension snapshots, environment configuration, or protocol messages.
6. Runtime theme changes cross the daemon/client boundary as renderer-neutral
   data. Ratatui types never cross that boundary.

## 3. Current state and problems

### 3.1 The current core binding

`core/src/ext/snapshots.rs` defines `LuaThemeSnapshot` with every supported
theme key as a Rust field. The Lua loader and TUI therefore share a compile-time
theme schema even though core does not depend on Ratatui.

Adding one theme role currently requires changes in several places:

1. `LuaThemeSnapshot` field;
2. Lua-table parsing;
3. `Theme::apply_snapshot`;
4. `Theme::set_highlight`;
5. documentation and tests.

This is the binding this plan removes.

### 3.2 The current theme is not app-wide

`tui/src/ui/theme.rs` covers only part of the transcript and bottom pane.
Independent styling currently exists in:

- `tui/src/ui/render/markdown.rs`;
- `tui/src/ui/picker.rs`;
- `tui/src/ui/stats.rs`;
- `tui/src/ui/jobs_pane.rs`;
- direct `Color::*` uses in message, prompt, stream, and pane rendering;
- the embedded Dark+ syntax theme.

This produces inconsistent appearance and prevents a usable light theme.

### 3.3 Runtime reset semantics are incorrect for configured themes

`Theme::set_highlight(name, None)` currently restores `Theme::default()`. If
`init.lua` configured that role, clearing a temporary highlight loses the
configured value. Runtime state needs a configured base plus an independent
override layer.

### 3.4 Dark-terminal assumptions are mixed

The main transcript often inherits the terminal background, while fullscreen
screens force an indexed black background. Markdown headings and syntax colors
also assume a dark background. A theme must make these decisions consistently.

## 4. Architectural decisions

### 4.1 Use an opaque protocol document

Add `protocol/src/theme.rs`:

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeDocument(pub BTreeMap<String, Value>);
```

`ThemeDocument` deliberately has no named theme fields. A map is used instead
of a typed palette because protocol and core must not own the TUI schema.
`BTreeMap` gives stable diagnostics and snapshot output.

Export it from `protocol/src/lib.rs`.

The document is suitable for the current in-process handoff and for later
transport in a `RuntimeEvent`. Introducing it now must not force completion of
the daemon-only Lua project.

### 4.2 Convert Lua generically in core

Replace `LuaThemeSnapshot` with `ThemeDocument` in the extension manager.

Core should iterate the root Lua table and convert string-keyed entries to
`serde_json::Value` using `LuaSerdeExt`. It may reject or skip values that are
not data, such as functions, userdata, or threads. It must not inspect key
names.

Conversion policy:

- root keys must be strings;
- strings, booleans, numbers, arrays, and nested string-keyed tables are
  accepted;
- unsupported entries are skipped with a path-specific warning;
- one malformed entry does not discard the entire theme;
- cycles and excessive nesting fail that entry safely;
- an absent `bone.theme` produces an empty document.

Core-level warnings describe conversion failures only. Semantic warnings such
as “unknown theme role” belong to the TUI resolver.

### 4.3 Separate unresolved, resolved, and live state in the TUI

Use three concepts:

```rust
/// Renderer-neutral values read from ThemeDocument.
struct ThemeSpec { /* TUI-owned parsed configuration */ }

/// Complete, valid styles used by renderers. No optional required roles.
struct ResolvedTheme { /* ratatui::Style and syntax selection */ }

/// Configured baseline plus temporary runtime overrides.
struct ThemeState {
    base: ResolvedTheme,
    active: ResolvedTheme,
    overrides: BTreeMap<ThemeRole, RuntimeOverride>,
}
```

Responsibilities:

- `ThemeSpec::parse(&ThemeDocument)` understands keys, aliases, and style
  values and returns diagnostics.
- `ResolvedTheme::resolve(preset, spec)` applies defaults and overrides and
  guarantees a usable value for every role.
- `ThemeState::set_highlight` modifies only the active layer.
- clearing a highlight copies that role from `base`, not from a built-in
  default;
- reloading the Lua configuration replaces `base`, resets `active` to the new
  base, and clears runtime overrides.

Do not expose mutable public color fields. Renderers should use immutable
accessors or `theme.active()` so all mutations pass through `ThemeState`.

### 4.4 Use semantic roles and component styles

The resolved theme should distinguish reusable palette values from styles that
need foreground/background pairs or modifiers.

Recommended initial palette:

```text
background
surface
surface_selected
text
text_muted
text_dim
border
accent
success
warning
error
```

Recommended component styles:

```text
transcript.user
transcript.assistant
transcript.system
tool.normal
tool.error
diff.added
diff.removed
diff.context
approval.safe
approval.danger
chrome.input_border
chrome.status
chrome.thinking
chrome.tab_active
selection.marker
selection.text
markdown.heading
markdown.link
markdown.inline_code
markdown.quote
markdown.rule
markdown.table_border
markdown.table_header
jobs.running
jobs.complete
jobs.error
charts.primary
charts.empty
charts.heatmap
```

Picker, catalog, setup, and stats should primarily consume the shared palette.
They should only gain component-specific roles when a palette role cannot
express the intended distinction. `charts.heatmap` is a preset-derived color
ramp rather than a single `Style`; truecolor presets may interpolate it while
the ANSI preset supplies discrete indexed buckets.

Use `ratatui::style::Style` for component styles. This fixes the current diff
problem where a background is configured without a guaranteed readable
foreground and allows modifiers to be part of the theme.

### 4.5 Preserve the runtime protocol initially

Keep this wire type unchanged during the first implementation:

```rust
ViewDiff::SetHighlight {
    name: String,
    fg: Option<String>,
}
```

The TUI maps legacy highlight names to resolved role properties. Canonical role
paths such as `chrome.input_border` may also be accepted, with the existing API
changing only that role's foreground. This avoids a protocol migration while
the app-wide theme work lands.

A future structured runtime API may add a separate variant:

```rust
ViewDiff::SetThemeStyle {
    name: String,
    style: Option<ThemeStyleDocument>,
}
```

Do not change the existing variant in place. Adding a new variant is safer for
older clients and preserves the current Lua API.

### 4.6 Make static theme transport ready for daemon-only Lua

Initially, the TUI can obtain `ThemeDocument` from the existing
`ExtensionManager` during boot. This is a source adapter, not a semantic
dependency: the TUI resolver only consumes the protocol document.

When the local TUI Lua VM is removed, deliver the same `ThemeDocument` as an
initial daemon/client event and after extension reload. No resolver or renderer
code should change at that point.

The eventual event should be additive:

```rust
RuntimeEvent::ThemeChanged {
    document: ThemeDocument,
}
```

The daemon must include the latest theme in late-join bootstrap events and emit
it after `ReloadExtensions`. Do not place Ratatui styles in `SessionSnapshot`.
The daemon will need a small shared client-bootstrap state holding the latest
opaque document; it must not resolve or validate the theme.

## 5. Configuration format

### 5.1 Existing flat format remains supported

Existing configurations continue to work:

```lua
bone.theme = {
    user_msg = "#ffffff",
    user_msg_bg = "#303030",
    status_text = "#808080",
    input_border = "#808080",
    system_msg = "#ffffff",
    approval_safe = "#78b373",
    approval_danger = "#e05050",
    tool_call = "#808080",
    tool_error = "#ff0000",
    diff_removed = "#870101",
    diff_added = "#005f00",
    thinking = "#8cdcdc",
    tab_active = "#8cdcdc",
}
```

Legacy mappings:

| Legacy key | New target |
|---|---|
| `user_msg` | `styles.transcript.user.fg` |
| `user_msg_bg` | `styles.transcript.user.bg` |
| `status_text` | `styles.chrome.status.fg` |
| `input_border` | `styles.chrome.input_border.fg` |
| `system_msg` | `styles.transcript.system.fg` |
| `approval_safe` | `styles.approval.safe.fg` |
| `approval_danger` | `styles.approval.danger.fg` |
| `tool_call` | `styles.tool.normal.fg` |
| `tool_error` | `styles.tool.error.fg` |
| `diff_removed` | `styles.diff.removed.bg` |
| `diff_added` | `styles.diff.added.bg` |
| `thinking` | `styles.chrome.thinking.fg` |
| `tab_active` | `styles.chrome.tab_active.fg` |

`tab_active` must either be wired to the active pane/tab indicator in this work
or removed in a separately announced breaking change. This plan wires it.

### 5.2 New structured format

```lua
bone.theme = {
    preset = "bone-dark",

    palette = {
        background = "#101214",
        surface = "#303438",
        surface_selected = "#3b4147",
        text = "#f1f3f5",
        text_muted = "#9aa1a9",
        text_dim = "#69717a",
        border = "#49515a",
        accent = "#8cdcdc",
        success = "#78b373",
        warning = "#e0a050",
        error = "#e05050",
    },

    styles = {
        transcript = {
            user = { fg = "#ffffff", bg = "#303438" },
        },
        diff = {
            added = { fg = "#eaffea", bg = "#165c2c" },
            removed = { fg = "#ffecec", bg = "#702525" },
        },
        markdown = {
            heading = { fg = "#ffffff", modifiers = { "bold" } },
            link = { fg = "#8cdcdc", modifiers = { "underlined" } },
        },
    },

    syntax = "dark-plus",
}
```

Resolution precedence, lowest to highest:

1. built-in fallback preset;
2. selected preset;
3. configured palette overrides;
4. component styles derived from the resulting palette;
5. legacy flat-key overrides;
6. structured `styles` overrides;
7. temporary runtime highlights.

If both a legacy key and its structured equivalent are present, the structured
value wins and one diagnostic is produced.

### 5.3 Color and style grammar

The TUI parser should support:

- named terminal colors already accepted today;
- `#RRGGBB` and `RRGGBB`;
- `default` for terminal foreground/background inheritance;
- `ansi:N` for explicit indexed colors where `N` is `0..=255`;
- modifiers: `bold`, `dim`, `italic`, `underlined`, `reversed`, and
  `crossed_out`.

Do not add alpha colors. Terminal backends cannot represent them consistently.
Do not interpret arbitrary CSS color syntax.

Invalid values fall back at the individual property level. One invalid style
must not discard the rest of the theme.

## 6. Built-in presets

Ship three presets:

1. `bone-dark`: preserves the current visual identity while removing direct
   color constants and supplying readable foreground/background pairs.
2. `bone-light`: defines all roles for a light terminal and selects a light
   syntax theme.
3. `bone-ansi`: uses only named or indexed terminal colors for terminals where
   truecolor is undesirable.

The default remains `bone-dark` for compatibility. Do not implement automatic
background detection in the first version. OSC background queries are not
portable enough to make startup behavior depend on them. Users can select a
preset explicitly.

Preset definitions belong in `tui`, preferably as Rust constants or embedded
data validated by TUI tests. They must not be seeded into core defaults.

## 7. Syntax highlighting

The current global Dark+ `LazyLock` must become a small TUI-owned registry:

```text
dark-plus  -> embedded dark_plus.tmTheme
light-plus -> embedded light_plus.tmTheme
```

`ResolvedTheme` stores a syntax-theme identifier, not a Syntect reference.
Markdown rendering looks up the cached Syntect theme using that identifier.

First-version constraints:

- allow built-in syntax themes only;
- select `dark-plus` from `bone-dark` and `bone-ansi`;
- select `light-plus` from `bone-light`;
- unknown names warn and use the preset default;
- syntax colors remain a TUI implementation detail.

Loading arbitrary filesystem theme files can be considered later. It adds path,
security, reload, and error-reporting concerns that are not required to make the
theme system modular.

## 8. Implementation phases

Each phase must compile and preserve a usable TUI.

### Phase 0 — Characterize current behavior

Before changing types:

- add focused render assertions for current user messages, tool rows, diffs,
  Markdown headings/code/tables, prompt selection, pane tabs, jobs, picker, and
  stats styles;
- document intentional current colors in the `bone-dark` preset fixture;
- add a characterization test recording the current built-in reset behavior,
  then update its expectation when Phase 3 fixes configured-base restoration;
- record allowed hard-coded colors used only by backend conversion and syntax
  adapters.

This avoids accidental visual changes being hidden inside the architecture
refactor.

### Phase 1 — Centralize TUI theme consumption

Keep the existing external Lua format and snapshot temporarily. Refactor the
TUI first:

1. Introduce `ResolvedTheme` with the complete palette and component roles.
2. Recreate the existing default as `bone-dark`.
3. Change render APIs to accept `&ResolvedTheme`:
   - `markdown::render_markdown(content, width, theme)`;
   - `jobs_pane::render(jobs, theme)`;
   - picker/setup/catalog draw functions;
   - stats run/draw functions;
   - pane conversion where a fallback style is needed.
4. Replace direct component `Color::*` uses with roles.
5. Keep direct colors only in:
   - theme preset definitions;
   - terminal backend color conversion;
   - Syntect-to-Ratatui conversion;
   - tests that intentionally assert parsing/conversion.
6. Wire `chrome.tab_active` into the active pane/tab indicator.

At the end of this phase, changing a constructed `ResolvedTheme` affects every
screen even though Lua still enters through the old snapshot.

### Phase 2 — Remove semantic theme knowledge from core

1. Add and export `bone_protocol::ThemeDocument`.
2. Replace `LuaThemeSnapshot` in `core/src/ext/snapshots.rs` with a generic
   table-to-document conversion helper.
3. Rename `ExtensionManager.theme_snapshot` to `theme_document`.
4. Rename `theme_snapshot()` to `theme_document()`.
5. Update boot and unloaded-manager construction to use an empty document.
6. Make TUI startup pass the document into `ThemeSpec::parse` and
   `ResolvedTheme::resolve`.
7. Move every semantic warning and fallback into the TUI resolver.
8. Delete the core list of theme keys.

At the end of this phase, adding a new static theme key requires no core change.

### Phase 3 — Add base/live state and structured configuration

1. Introduce `ThemeState { base, active, overrides }`.
2. Implement legacy alias resolution.
3. Implement the structured palette/style schema.
4. Change `set_highlight(name, Some(color))` to update the active role only.
5. Change `set_highlight(name, None)` to restore that role from `base`.
6. Clear runtime overrides when extensions/theme configuration reloads.
7. Ensure invalid runtime colors leave the current active value unchanged.
8. Preserve the existing `ViewDiff::SetHighlight` wire representation.

### Phase 4 — Add light/ANSI presets and syntax-theme selection

1. Add `bone-light` and `bone-ansi` presets.
2. Embed and cache a light syntax theme.
3. Add preset selection and fallback diagnostics.
4. Ensure fullscreen root blocks use the resolved background.
5. Verify foreground/background pairs under every preset.

### Phase 5 — Prepare daemon delivery

This phase may land with the daemon-only Lua work rather than the initial theme
PRs.

1. Add `RuntimeEvent::ThemeChanged { document }`.
2. Store the latest opaque document in daemon client-bootstrap state.
3. Include `ThemeChanged` in late-join initial events.
4. Emit it after extension reload.
5. Apply it in the TUI by rebuilding `ThemeState` and forcing a redraw.
6. Remove the TUI's direct read from `ExtensionManager` when the TUI-local Lua
   VM is removed.

This phase changes the theme source only. Theme parsing and rendering remain
unchanged.

## 9. File-level change map

### New files

| File | Responsibility |
|---|---|
| `protocol/src/theme.rs` | Opaque serializable `ThemeDocument` |
| `tui/src/ui/theme/mod.rs` | Public theme types and state |
| `tui/src/ui/theme/spec.rs` | Document parsing, aliases, diagnostics |
| `tui/src/ui/theme/presets.rs` | Dark, light, and ANSI presets |
| `tui/src/ui/theme/resolve.rs` | Precedence and complete style resolution |
| `tui/src/ui/render/themes/light_plus.tmTheme` | Light syntax highlighting |
| `tui/tests/theme_test.rs` | Resolver, compatibility, and state tests |

The existing `tui/src/ui/theme.rs` can be converted to `theme/mod.rs` when the
implementation becomes large enough. Do not split it prematurely if the first
phase remains readable in one file.

### Core changes

| File | Change |
|---|---|
| `core/src/ext/snapshots.rs` | Generic Lua table to `ThemeDocument`; delete named theme fields |
| `core/src/ext/loader.rs` | Collect a document instead of a typed snapshot |
| `core/src/ext/types.rs` | Store/expose `theme_document` |
| `core/src/ext/api_ui.rs` | No initial wire change; keep generic named highlight updates |
| `core/defaults/AGENTS.md` | Document presets, structured styles, aliases, and reset behavior |
| core tests | Assert generic conversion, not recognized theme names |

### Protocol changes

| File | Change |
|---|---|
| `protocol/src/lib.rs` | Export `ThemeDocument` |
| `protocol/src/theme.rs` | New opaque type and serde tests |
| `protocol/src/event.rs` | Only Phase 5 adds `ThemeChanged` |
| `protocol/src/view.rs` | Keep `SetHighlight` unchanged initially |

### TUI changes

| File or area | Change |
|---|---|
| `ui/theme` | Own all semantics, presets, state, validation |
| `ui/color.rs` | Parse `default` and `ansi:N`; remain TUI-only |
| `ui/render/markdown.rs` | Consume theme roles and select cached syntax theme |
| `ui/render/messages.rs` | Use complete transcript and diff styles |
| `ui/render/bottom_pane.rs` | Remove remaining direct colors; use selection/tab roles |
| `ui/render/mod.rs` | Store `ThemeState`; pass active theme to renderers |
| `ui/jobs_pane.rs` | Accept theme and use job status roles |
| `ui/picker.rs` | Replace exported color constants with palette access |
| `ui/setup.rs` | Pass theme through draw functions |
| `ui/catalog.rs` | Pass theme through draw functions |
| `ui/stats.rs` | Pass theme through run/draw functions and charts |
| `ui/pane_page.rs` | Use theme fallback styles while preserving explicit plugin colors |
| `ui/app/mod.rs` | Resolve boot document; apply runtime overrides and reloads |
| `main.rs` | Supply a built-in theme to first-run setup and resolved themes to standalone screens where available |

## 10. Fullscreen and standalone behavior

Fullscreen modules must not load configuration themselves. Their APIs receive a
resolved theme from the caller:

```rust
pub fn run<F>(theme: &ResolvedTheme, load: F) -> io::Result<()>;
```

Rules:

- app-launched stats, setup, and catalog receive `app.theme.active()`;
- the first-run setup wizard uses `bone-dark` because no `init.lua` may exist;
- standalone commands resolve a `ThemeDocument` in orchestration code when one
  is already available;
- no fullscreen module boots Lua merely to obtain colors;
- Phase 5 lets standalone clients receive the same opaque document from the
  daemon bootstrap path.

## 11. Plugin-provided explicit colors

`PaneSpanSpec` and `StatusSegment` can already carry explicit color strings.
Those values are content-level overrides and should remain supported.

Precedence for plugin UI:

1. explicit span/line/status color supplied by the plugin;
2. component theme role;
3. terminal default.

The TUI parses explicit colors. Core and protocol continue to carry strings.
Invalid plugin colors fall back to the relevant theme role rather than to a
hard-coded color.

This plan does not add semantic palette references to plugin payloads. A later
protocol extension could support values such as `role:accent`, but that needs
cross-client semantics and should be designed separately.

## 12. Diagnostics and validation

Theme resolution returns a complete theme plus diagnostics:

```rust
struct ThemeResolution {
    theme: ResolvedTheme,
    diagnostics: Vec<ThemeDiagnostic>,
}
```

Diagnostics should include:

- unknown preset;
- unknown key or style role;
- invalid color or modifier;
- wrong value type;
- a structured property overriding a legacy alias;
- unknown syntax theme;
- low contrast for explicit RGB foreground/background pairs.

Diagnostics are non-fatal. They should be emitted once per load/reload, not on
every render frame. Contrast checks are advisory because named/indexed colors
depend on the terminal palette. Only explicit RGB pairs can be evaluated
reliably.

## 13. Test plan

### 13.1 Protocol tests

- empty `ThemeDocument` round trip;
- nested map/array round trip;
- stable JSON representation;
- future unknown fields survive a round trip unchanged.

### 13.2 Core tests

- absent `bone.theme` produces an empty document;
- a flat string-valued table is copied without core knowing key semantics;
- nested palette/style tables are preserved;
- unsupported Lua values are skipped without losing valid siblings;
- unloaded extension manager exposes an empty document;
- no core test asserts a semantic theme key as supported or unsupported.

### 13.3 Resolver tests

- each built-in preset resolves every required role;
- legacy flat configuration produces the same resolved styles as before;
- structured styles override legacy aliases;
- invalid individual properties fall back without discarding valid siblings;
- `default`, named, RGB, and indexed colors parse correctly;
- unknown presets and syntax themes fall back predictably;
- clearing a runtime override restores the configured base value;
- clearing one runtime override does not clear unrelated overrides;
- reload replaces the base and clears runtime overrides.

### 13.4 Rendering tests

Render representative buffers under `bone-dark` and `bone-light`:

- user, assistant, system, and tool messages;
- added, removed, and context diff lines;
- Markdown headings, links, inline code, fenced code, quotes, rules, and tables;
- approval and prompt selection;
- active/inactive pane tabs;
- running/completed/error jobs;
- setup/catalog picker selection;
- stats panels, bars, heatmap, and error overlay.

Assertions should inspect `Style` foreground/background/modifiers, not terminal
escape sequences.

### 13.5 Boundary tests

- `bone-core` builds without Ratatui;
- `bone-protocol` builds without Lua or terminal dependencies;
- adding an unknown theme key requires no core change and reaches the TUI
  document intact;
- `ViewDiff::SetHighlight` retains JSON compatibility;
- Phase 5: late-joining clients receive the latest document;
- Phase 5: extension reload emits one new theme document and the TUI redraws.

### 13.6 Hard-coded color audit

CI or a documented review command should flag new direct UI colors outside an
allowlist:

```bash
rg -n 'Color::(Rgb|Indexed|White|Black|Red|Green|Yellow|Blue|Magenta|Cyan|Gray|DarkGray)' \
  tui/src/ui --glob '*.rs'
```

Allowed locations are preset definitions, color conversion, the terminal
backend, and syntax conversion. Every other hit requires justification.

## 14. Compatibility and migration

### User configuration

- all existing flat keys continue to work;
- missing keys retain defaults;
- invalid keys remain non-fatal;
- `bone.api.ui.set_highlight(name, color_or_nil)` remains available;
- `nil` changes behavior intentionally: it restores the configured base rather
  than the compiled default;
- the default preset should visually approximate the current UI.

### Wire protocol

- Phases 0–4 do not change `ViewDiff::SetHighlight`;
- `ThemeDocument` is additive;
- Phase 5 adds a new runtime event rather than changing existing events;
- clients that do not implement `ThemeChanged` need the normal protocol
  version/unknown-variant policy before mixed-version support can be claimed.

### Internal Rust API

Render function signatures will change to accept a theme. This is intentional
and compiler-guided. Avoid global theme singletons; explicit parameters make
tests deterministic and keep components reusable.

## 15. Delivery strategy

Recommended pull-request sequence:

1. **PR 1 — Theme coverage:** introduce resolved roles, reproduce the current
   dark appearance, and route every TUI component through the theme.
2. **PR 2 — Boundary cleanup:** add `ThemeDocument`, remove semantic theme keys
   from core, and add the TUI resolver with legacy aliases.
3. **PR 3 — Theme features:** add structured styles, correct runtime reset
   semantics, light/ANSI presets, syntax selection, diagnostics, and docs.
4. **PR 4 — Daemon delivery:** add `ThemeChanged` alongside the daemon-only Lua
   work and remove the TUI's direct extension-manager read.

Do not combine daemon ownership changes with the initial rendering refactor.
The serializable document is the seam that lets those efforts proceed
independently.

## 16. Definition of done

The work is complete when:

- every first-party UI surface consumes one resolved theme;
- a light preset renders without dark-only hard-coded text or backgrounds;
- diff foreground/background pairs are always defined and readable;
- runtime highlight reset restores the configured base;
- existing flat `bone.theme` configurations still work;
- adding a new theme role requires changes only in TUI theme/rendering code and
  user documentation, not core Lua parsing;
- `bone-core` Rust code contains no semantic theme-key list and no Ratatui
  dependency;
- `bone-protocol` contains only opaque serializable theme data;
- all theme, render, protocol, and workspace tests pass;
- the theme source can later move from local `ExtensionManager` access to a
  daemon event without changing the resolver or renderers.
