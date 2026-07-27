# TUI theme revamp plan

## Goal

Make every user-visible TUI surface use one coherent semantic theme, preserve configured values when runtime overrides are cleared, and remove schema/runtime behavior that appears supported but is not implemented.

The end state is one configured base theme, one sparse runtime-override layer, and one effective `Theme` consumed by every native renderer. Local and remote TUIs must derive the same effective result from the same daemon-owned state.

## Principles

- Keep `Theme` as the single resolved TUI styling model. A small layering/state wrapper is acceptable; a second palette model is not.
- Use semantic roles, not component-specific raw colors.
- Pass `&Theme` explicitly at renderer boundaries; do not use globals or make terminal lifecycle code choose styling.
- Keep terminal/backend RGB conversion constants separate from visible styling concerns.
- Preserve current defaults unless a change is intentional and covered by a rendering test.
- Resolve configuration once, then layer temporary runtime overrides without mutating or flattening them into the configured base.
- Avoid one role per widget when existing roles express the same meaning.
- Preserve extension-authored `PaneContent` colors, but make unstyled extension content inherit the active foreground.
- Remove unsupported configuration instead of silently accepting it.
- Do not expand accepted color syntax as a side effect of fixing conversion behavior.
- Preserve unrelated working-tree changes while implementing this plan.

## Current source audit

This plan was refreshed against `main` after the background-activity refactor (`ffb4694`). Important changes and verified gaps:

- Jobs and processes now share `tui/src/ui/selectable_pane.rs`; its marker, foregrounds, and selected background are hardcoded. Theme that shared helper once instead of duplicating styling in both panes.
- `tui/src/ui/process_view.rs` is a new fullscreen surface with hardcoded command, stdout, stderr, metadata, and footer colors. It is in scope.
- `/stats` still exists through direct, popup, and in-app entry points. There is no `SIMPLIFICATION_PLAN.md` in the current tree, so this plan must not assume stats will be deleted.
- `ResolvedFrontendSettings::frontend_settings()` currently flattens daemon runtime highlights into `settings.theme`. That destroys the configured/temporary distinction in full snapshots and must change before reset semantics can be correct.
- `Theme::set_highlight(..., None)` still restores `Theme::default()` rather than the configured value.
- Persisted theme validation currently validates only `theme.name`; invalid colors and unsupported highlight behavior reach the renderer and only warn.
- `ThemeStyleSpec` accepts `bold`, `italic`, and `underline`, but the TUI discards them. `tab_active` is stored and mutable but has no renderer caller.
- `input_bg`, `input_prefix`, and `input_cursor` are real renderer roles exposed only through `theme.highlights`; that is a valid single path if documented and validated.
- Configured/runtime theme strings currently accept named colors and six-digit RGB hex only. `Color::Indexed` and `Color::Reset` can reach defensive conversion helpers, but are not accepted theme syntax.
- Fullscreen picker/setup/catalog/stats use independent palettes. `process_view` is also fullscreen but bypasses that shared picker palette.
- Markdown receives only the syntect theme, so structural Markdown cannot use app roles until its API receives the app `Theme` as well.
- Native `PanePage` renderers, transcript footer, messages, prompt/autocomplete states, and the thinking pane still contain visible hardcoded colors.
- `PaneContent` may intentionally carry extension-authored explicit colors. Those values are content data, not native palette constants, and should not be rewritten to semantic roles.
- The working tree was clean when this refresh began; the old file-specific uncommitted-change warning is no longer current.

## Non-goals

- Redesigning the daemon/frontend ownership model beyond separating configured theme data from runtime overrides.
- Replacing ratatui, syntect, or the fullscreen lifecycle abstraction.
- Making extension-authored pane colors automatically remap to semantic roles.
- Introducing a generic style registry, global theme singleton, widget-specific palette structs, or compatibility layer for settings that never worked.
- Changing layout, copy, key handling, pane ordering, or process/job behavior while migrating styles.

## Status

- [x] 0. Confirm scope and product decisions
- [x] 1. Fix configured theme and runtime override semantics
- [x] 2. Freeze the role contract and align schema
- [x] 3. Complete theme propagation through main-app surfaces
- [x] 4. Theme fullscreen and pre-app surfaces
- [x] 5. Add semantic Markdown styling
- [x] 6. Centralize and harden color conversion
- [x] 7. Add cross-path propagation and regression coverage
- [ ] 8. Run final audit, validation, and cleanup

---

## 0. Confirm scope and product decisions

**Target:** Settle choices that affect the shape of the implementation before adding roles or changing APIs.

### Decisions and recommended defaults

Resolve these before changing public schema. Unless product direction changes, use the recommendation shown:

- [x] **Stats:** retain and theme it because all three entry paths still exist. If stats is separately approved for deletion before this phase starts, remove it and omit data-visualization roles instead.
- [x] **Structured modifiers:** remove `bold`, `italic`, and `underline` from `ThemeStyleSpec`. They have never reached the renderer, and supporting them would require changing color fields into style-bearing role values throughout the TUI. If configurable typography is explicitly desired, revise this plan first and implement that role-model change end to end rather than partially honoring modifiers.
- [x] **Unused role:** remove `tab_active` unless a concrete current tab surface is identified. Do not retain it as a speculative compatibility field.
- [x] **Input roles:** keep `input_bg`, `input_prefix`, and `input_cursor` as validated/documented entries in `theme.highlights`; do not add duplicate flat fields.
- [x] **Pre-app theme source:** add one helper that resolves `Theme` from canonical settings after `seed_base()` and before fullscreen entry. Use `Theme::default()` only when configuration is unavailable, and surface a warning for invalid/load-failure fallback.
- [x] **Accepted color syntax:** keep the public syntax at named colors plus `#RRGGBB`/`RRGGBB`. Reject `Indexed` and `Reset` in OSC/syntect conversion paths rather than inventing a fixed RGB value or silently broadening configuration syntax.
- [x] **Explicit pane colors:** preserve valid colors supplied by `PaneContent`; apply the active `fg` only to unstyled spans/lines and active `bg` only at the containing surface.
- [x] **Stats roles if retained:** use a compact data-visualization set such as `chart`, `chart_empty`, `heat_low`, and `heat_high`, derived from existing palette roles by default. Generate gradients from endpoints instead of exposing fifteen color slots.

### Acceptance

- [x] Each decision is reflected in later phases, schema, tests, and documentation.
- [x] No placeholder role, ignored field, stale simplification-plan reference, or compatibility layer remains.
- [x] The final role list is written down before renderer signatures and public schema are changed.

---

## 1. Fix configured theme and runtime override semantics

**Target:** Clearing a temporary runtime override restores the active configured theme, not `Theme::default()`.

### Design

Maintain three explicit pieces of frontend state:

1. **Configured base theme** — rebuilt from the latest resolved settings snapshot.
2. **Runtime overrides** — a sparse `name -> color` map owned authoritatively by `ViewModel.highlights` and mirrored by the TUI for incremental rendering.
3. **Effective theme** — a `Theme` rebuilt by applying the sparse overrides over the configured base.

A `None` runtime value removes the map entry and exposes the configured value beneath it. Theme reloads replace the base and then reapply remaining overrides. Do not mutate the base in `set_highlight`, and do not recover it from `Theme::default()`.

Full snapshots must preserve this distinction on the wire. `ExtRuntime::frontend_settings()` currently inserts runtime values into `settings.theme.palette/highlights`; replace that flattening with an explicit runtime-highlight field in the resolved frontend snapshot (or an equivalently separate protocol field). Snapshot application must always process configured settings first and runtime overrides second.

Keep ownership authoritative:

- The daemon owns the canonical sparse runtime override map in `ViewModel.highlights`.
- Resolved frontend snapshots carry configured theme settings and runtime overrides as separate values.
- The TUI mirrors the override map so incremental `SetHighlight` diffs, reconnect snapshots, and later settings reloads all produce the same effective theme.
- The renderer consumes only the effective `Theme`; it does not query daemon state or configuration.

### Work

- [x] Introduce the smallest TUI state holder needed for configured `Theme`, sparse overrides, and effective `Theme`; avoid duplicating role definitions.
- [x] Stop merging runtime highlights into `settings.theme` in `core/src/ext/types.rs`.
- [x] Extend the resolved frontend snapshot shape and serde/protocol tests so configured settings and runtime highlights remain distinct across local and remote paths.
- [x] Make snapshot application establish/replace the configured base, replace the mirrored override map with the snapshot map, then derive the effective theme once.
- [x] Make incremental `SetHighlight(Some(color))` update the sparse map and recompute only the affected effective role where practical.
- [x] Make incremental `SetHighlight(None)` remove the override and restore the corresponding configured role, including configured absence for optional roles.
- [x] Resolve runtime values with the same color parser and role-name validation used by persisted highlights; runtime palette references remain unsupported unless deliberately added to both API and documentation.
- [x] Ensure clearing `bg` restores the configured background and emits the correct OSC background transition.
- [x] Rebuild syntect only when an effective `syntax_*` value actually changes; applying an unrelated override must not rebuild it.
- [x] Ensure an invalid/unknown runtime update does not mutate either the override map or effective theme.
- [x] Clarify API documentation: Lua `nil` removes the runtime override; it does not mean “built-in default” and does not persist a change.

### Primary files

- `tui/src/ui/theme.rs`
- `tui/src/ui/render/mod.rs` or the renderer state owner selected during implementation
- `tui/src/ui/app/mod.rs`
- `core/src/runtime/view.rs`
- `core/src/ext/types.rs`
- `core/src/ext/snapshots.rs`
- `core/src/ext/api_ui.rs`
- protocol/snapshot tests touched by the resolved frontend settings shape

### Tests

- [x] Configured foreground → runtime override → reset restores configured foreground.
- [x] Configured background → runtime override → reset restores configured background and terminal state.
- [x] Configured absent background → runtime override → reset issues terminal background reset.
- [x] Configured optional role absence remains absent after override removal.
- [x] Theme reload under an active override changes the hidden base and exposes the new value after reset.
- [x] Runtime syntax reset restores configured syntax and rebuilds code highlighting exactly once.
- [x] Unrelated override changes do not rebuild code highlighting.
- [x] A reconnect/full snapshot and the equivalent incremental diff sequence produce equal configured, override, and effective state.
- [x] Invalid colors and unknown roles leave all three states unchanged.
- [x] Runtime overrides never appear in serialized persisted theme settings.

### Patch boundary and rollback

Land daemon snapshot separation and the TUI layering holder as one runtime/protocol patch, with no new roles or renderer migrations. Change producers, consumers, and serde tests atomically so every commit compiles; do not add a dual snapshot format unless the project's protocol policy requires one. Rollback restores the former flattened snapshot behavior without touching later schema or renderer patches.

### Acceptance

- [x] No reset path restores a built-in value when a configured value exists.
- [x] Snapshot, reconnect, settings-reload, and incremental-diff paths produce the same effective theme.
- [x] Runtime overrides remain temporary, separately represented, and never overwrite persisted or configured theme data.
- [x] Local in-process and remote TUI behavior is equivalent.

---

## 2. Freeze the role contract and align schema

**Target:** Establish one authoritative role registry before renderer signatures proliferate, and make configuration fail at validation rather than degrade into renderer warnings.

**Depends on:** Phase 0 decisions. This phase may consume the phase 1 state holder, but must remain a schema/contract patch rather than absorbing renderer migrations.

### Canonical role registry

Define one data-only registry in `core` and consume it from settings validation, resolved-snapshot/runtime validation, generated documentation, and registry-completeness tests. The TUI maps those registered names to `Theme` fields; it must not maintain a second handwritten accepted-name list in `set_named_color`.

Each entry records at least:

- public name and short description;
- role kind: foreground, background, or composite;
- whether absence is valid (`bg` only unless phase 0 approves another optional role);
- built-in fallback or palette role from which the default is derived;
- whether a change invalidates the cached syntect theme;
- supported persisted forms and runtime mutability.

Freeze this role set:

- Palette: `bg`, `fg`, `muted`, `subtle`, `border`, `accent`, `good`, `warn`, `error`, `selection`.
- Existing UI: `user_msg`, `user_msg_bg`, `status_text`, `input_border`, `input_bg`, `input_prefix`, `input_cursor`, `system_msg`, `approval_safe`, `approval_danger`, `tool_call`, `tool_error`, `diff_removed`, `diff_added`, and `thinking`.
- Shell: `shell_program`, `shell_separator`, `shell_redirect`, `shell_flag`, `shell_string`, `shell_variable`, `shell_comment`, and `shell_path`.
- Syntax: `syntax_text`, `syntax_comment`, `syntax_string`, `syntax_number`, `syntax_constant`, `syntax_escape`, `syntax_regex`, `syntax_keyword`, `syntax_keyword_control`, `syntax_type`, `syntax_function`, `syntax_variable`, `syntax_tag`, `syntax_attribute`, `syntax_punctuation`, `syntax_subtle`, `syntax_markup`, and `syntax_invalid`.
- Markdown: `markdown_marker`, `markdown_heading`, `markdown_link`, `markdown_inline_code`, `markdown_rule`, `markdown_table_border`, and `markdown_table_header`.
- Stats, if retained under phase 0: `chart`, `chart_empty`, `heat_low`, and `heat_high`.
- Remove `tab_active` unless phase 0 identifies an actual consumer.

Derive new defaults without changing the built-in palette: `markdown_marker` → `muted`, `markdown_heading` → `fg`, `markdown_link`/`markdown_inline_code` → `muted`, `markdown_rule` → `subtle`, `markdown_table_border` → `border`, `markdown_table_header` → `accent`; retained stats use `chart` → `accent`, `chart_empty`/`heat_low` → `subtle`, and `heat_high` → `good`. If closer visual parity requires a different existing palette fallback, record that phase 0 decision before schema lands rather than adding another raw default.

Do not add process-, job-, queue-, picker-, or pane-specific roles: those surfaces can use the palette and existing status roles.

### Foreground/background contract

- Palette entries are configured as scalar values under `theme.palette`; `bg` may be absent. They are not also accepted as `theme.highlights` groups. Of these names, only the already-supported `bg` is runtime-mutable; reject runtime updates to `fg`, `muted`, `subtle`, `border`, `accent`, `good`, `warn`, `error`, and `selection`.
- Within `theme.highlights`, foreground roles accept a scalar color/reference or `{ fg = ... }`; reject `{ bg = ... }`.
- Highlight background roles are `user_msg_bg` and `input_bg`. They accept a scalar color/reference or `{ bg = ... }`; reject `{ fg = ... }`.
- `user_msg` remains the sole composite highlight group for compatibility: `fg` targets `user_msg`, and `bg` targets `user_msg_bg`. The explicit `user_msg_bg` name remains the unambiguous single-channel form.
- A style object with neither supported channel is invalid. Unsupported keys are rejected by serde after removing modifier fields.
- Color references are limited to palette names. Do not permit role-to-role chains, cycles, indexed values, `Reset`, or arbitrary extension group names.
- Runtime `SetHighlight` remains a single-color operation against an exact runtime-mutable registry entry: `bg` plus the UI, shell, syntax, Markdown, and retained-stats names. `None` removes the override. Runtime updates cannot address the composite `user_msg` background channel indirectly—use `user_msg_bg`.
- Unknown `theme.highlights` names fail persisted validation; unknown or non-runtime-mutable names reject runtime updates before any state mutation. Extension-authored `PaneContent` colors are unrelated content data and do not create registry entries.

### Schema and documentation work

- [x] Remove `bold`, `italic`, and `underline` from `ThemeStyleSpec` as decided in phase 0; do not silently deserialize and discard them.
- [x] Remove `tab_active` from settings, `Theme`, mutation code, docs, and tests unless phase 0 retained it with a named renderer consumer.
- [x] Keep `input_bg`, `input_prefix`, and `input_cursor` only in `theme.highlights`, with registry-backed validation and documentation.
- [x] Preserve current structured sections and supported legacy flat fields for existing working settings. Document exact precedence: defaults → palette → derived roles → structured shell/syntax fields → supported legacy flat fields → `highlights`; runtime overrides are later and never persisted.
- [x] Move the accepted string syntax into a backend-neutral `core` parser/value (`named` or RGB) used by settings and runtime validation; make the TUI adapter convert that parsed value to ratatui `Color`. This is a color value contract, not a second palette/theme model.
- [x] Validate every configured palette, structured, legacy, and highlight color through that parser. Emit a field path and rejected value.
- [x] Validate palette references only where references are currently supported; do not accidentally treat an unknown string as a future extension role.
- [x] Add direct parser tests for every accepted named form, `#RRGGBB`, `RRGGBB`, case handling, malformed hex, indexed-looking strings, empty strings, and `reset`.
- [x] Generate the theme-role section in `core/defaults/AGENTS.md` from registry metadata as part of default-document generation, and add a test that the checked-in/generated output is current.
- [x] Document Lua reset semantics: `nil` removes a temporary override and reveals configured state.

### Primary files

- `core/src/config/settings.rs`
- `core/src/ext/api_ui.rs`
- `core/defaults/AGENTS.md`
- `tui/src/ui/color.rs`
- `tui/src/ui/theme.rs`
- focused config, serde, and role-registry tests

### Focused tests

- [x] Registry names are unique, every TUI `Theme` role has exactly one mapping, and every documented name is registered.
- [x] Foreground, background, and composite channel matrices accept and reject the exact forms above.
- [x] Every persisted color-bearing path reports invalid input during settings validation.
- [x] Removed modifiers and `tab_active` fail as unknown configuration instead of being ignored.
- [x] Runtime unknown names and invalid colors are rejected without emitting a diff or changing daemon/TUI state.
- [x] Legacy precedence tests remain green for fields deliberately retained.

### Patch boundary and rollback

Land this as one contract/schema patch with migration notes. It may add dormant Markdown/stats fields to `Theme`, but must not migrate renderers or change runtime layering. Rollback restores the old schema and registry mapping without reverting phase 1.

### Acceptance

- [x] Validation, snapshots, runtime application, documentation, and tests consume one role-name source.
- [x] Every role has an exact foreground/background capability and a tested failure point.
- [x] No accepted field is ignored, and no renderer warning is the first line of defense for persisted settings.

---

## 3. Complete theme propagation through main-app surfaces

**Target:** Remove semantic styling bypasses from the normal app, including shared pane infrastructure, while preserving line backgrounds and extension-authored span colors.

**Depends on:** Phases 1–2. Use only the frozen role contract; do not add schema in this renderer patch.

### Mapping

- Normal text → `fg`; secondary text, timestamps, metadata, and inactive markers → `muted` or `status_text`; structural separators → `subtle`/`border`.
- Selected row background → `selection`; selected marker → `accent`; selected row foreground stays span-specific or inherits `fg`.
- Running/active → `accent`, queued/caution → `warn`, completed/success → `good` where success is meaningful, failed/stderr → `error`.
- Tool/shell/diff/thinking content continues to use its existing dedicated role when one exists.

### Work

- [x] Change `selectable_pane::render` to accept `&Theme` and remove `SELECTED_BG`, white selected markers, and dark-gray inactive markers. Migrate jobs and processes through this helper once.
- [x] Pass `&Theme` into jobs, processes, and queue row construction; replace local status-color functions and direct white/gray/yellow/red choices with the mapping above.
- [x] Preserve `bottom_pane` line-level backgrounds through full-row `Paragraph` styles so selected rows remain edge-to-edge after migration.
- [x] Pass `&Theme` through `PanePage`/pane rendering boundaries where native pages need a containing foreground/background. Do not overwrite an explicit `Span`/`Line` foreground from `PaneContent`; only unstyled content inherits `theme.palette.fg`.
- [x] Use `thinking` for the title/semantic thinking accent and `muted` for the current hardcoded thinking body in `app/stream/mod.rs`.
- [x] Theme the transcript footer, tool names, shell label, prompt options, autocomplete selection, queue footer, and other main-app native labels.
- [x] Ensure messages and prompt rendering receive the same effective `&Theme` already owned by the renderer rather than cloning a palette or consulting settings.
- [x] Preserve modifiers, explicit extension colors, and per-span styles while wrapping or applying containing row styles.

### Primary files

- `tui/src/ui/selectable_pane.rs`
- `tui/src/ui/jobs_pane.rs`
- `tui/src/ui/processes_pane.rs`
- `tui/src/ui/queue_pane.rs`
- `tui/src/ui/pane_page.rs`
- `tui/src/ui/app/stream/mod.rs`
- `tui/src/ui/transcript_view.rs`
- `tui/src/ui/render/messages.rs`
- `tui/src/ui/render/bottom_pane.rs`
- `tui/src/ui/render/mod.rs`

### Focused tests

- [x] A distinct test theme controls selected markers/backgrounds in both jobs and processes through `selectable_pane`.
- [x] Queue selected rows and bottom-pane options retain edge-to-edge backgrounds.
- [x] Statuses map to the expected semantic roles without asserting built-in RGB values.
- [x] Thinking body, transcript footer, tool/shell labels, and autocomplete use the supplied theme.
- [x] Unstyled extension pane content inherits `fg`; explicitly colored extension spans remain unchanged after wrapping and selection/containing styles.

### Patch boundary and rollback

Prefer two renderer-only patches: shared/native panes first, transcript/messages/prompt second. Neither patch changes protocol, settings, or role names. Each can be reverted independently to restore old colors without affecting phase 1 layering or phase 2 schema.

### Acceptance

- [x] These renderers contain no visible raw palette constants or direct `Color::*` choices except deliberate content colors classified in phase 8.
- [x] Selection, warning, error, success, muted, normal, and extension-authored content behavior is test-covered.
- [x] No unrelated layout, key handling, copy, or process/job behavior changes.

---

## 4. Theme fullscreen and pre-app surfaces

**Target:** Picker, setup, catalog, stats, and the new process viewer consume the same resolved application theme without moving theme policy into terminal lifecycle code.

**Depends on:** Phases 1–2. Independent of phase 3 renderer migration.

### Theme source and propagation

- After `seed_base()`, resolve canonical frontend settings once at each startup path that can enter setup/catalog/picker before normal app construction, build the configured base `Theme`, and pass `&Theme` into the fullscreen surface.
- In-app fullscreen entry points receive the current effective `Theme`, including sparse runtime overrides. A remote TUI uses its latest snapshot-derived effective theme; it must not reread local config.
- If startup settings are unavailable or invalid, warn once and use `Theme::default()` for that invocation. Do not silently mix partially applied invalid settings with defaults.
- `fullscreen::run` and `FullscreenTerminal` remain responsible only for terminal setup, drawing, and teardown.

### Work

- [x] Replace the exported indexed palette in `picker.rs` with `&Theme` parameters and semantic role use in picker, setup, and catalog widgets.
- [x] Thread the theme through startup-time and in-app setup/catalog/picker callers in `tui/src/main.rs` and app command handling.
- [x] Pass `&Theme` into `process_view::run`, `run_loop`, `process_lines`, and `draw`; map command text to `fg`, prompt/footer/metadata to `muted`, stdout to `fg`, and stderr/process errors to `error` while preserving intentional bold command typography.
- [x] Retain and migrate stats unless phase 0 separately approved deletion. Use `chart`/`chart_empty`; interpolate heatmap RGB values between `heat_low` and `heat_high`; use `error` for load failures. Keep data and layout unchanged.
- [x] Apply the theme background/foreground to containing fullscreen widgets without introducing a second palette struct.
- [x] Remove picker/stats/process-view local palette constants only after all their callers accept a theme.

### Primary files

- `tui/src/main.rs`
- `tui/src/ui/picker.rs`
- `tui/src/ui/setup.rs`
- `tui/src/ui/catalog.rs`
- `tui/src/ui/stats.rs`
- `tui/src/ui/process_view.rs`
- `tui/src/ui/fullscreen.rs`
- app command/fullscreen call sites

### Focused tests

- [x] Startup setup/catalog/picker use configured colors resolved after seeding.
- [x] In-app and remote fullscreen entry points use the current effective theme, including an active runtime override.
- [x] Failed pre-app resolution warns and uses one complete default theme.
- [x] Process command, stdout, stderr, metadata, and footer styles come from the supplied unusual theme.
- [x] Stats bars, empty cells, error state, and heatmap endpoints respond to the four registered roles; interpolation is deterministic.
- [x] Fullscreen terminal setup/teardown behavior is unchanged.

### Patch boundary and rollback

Land pre-app resolution/API plumbing first, then fullscreen renderer migration. Do not combine this with main-app or Markdown migration. The plumbing patch is rollback-safe because callers can temporarily pass `Theme::default()`; the renderer patch can then be reverted independently.

### Acceptance

- [x] Deliberately unusual configured and runtime-overridden themes are visible on every fullscreen entry path.
- [x] No independent picker, stats, or process-view palette remains.
- [x] `fullscreen.rs` contains no config loading, role mapping, or fallback policy.

---

## 5. Add semantic Markdown styling

**Target:** Structural prose and fenced code both derive from the active application theme without losing span styles during wrapping and table layout.

**Depends on:** Phase 2 role/schema contract. Independent of phases 3–4 renderer migrations.

### Work

- [x] Change `render_markdown` and `MarkdownRenderer` to receive `&crate::ui::theme::Theme`, not only `&syntect::highlighting::Theme`.
- [x] Use `Theme::code()` only when constructing `HighlightLines` for fenced code.
- [x] Replace `MUTED`, `INLINE_CODE`, `CODE_FALLBACK`, `RULE`, `TABLE_BORDER`, `TABLE_HEADER_FG`, direct heading white, and direct link gray with the registered Markdown roles or documented `fg`/`muted` fallback.
- [x] Keep semantic typography—heading bold/underline, emphasis, strikethrough, and syntect font styles—unchanged. Phase 0 removes configurable modifiers, not renderer-authored Markdown semantics.
- [x] Pass the application theme through every Markdown call site in message/transcript rendering.
- [x] Refactor prefix detection so wrapping identifies prefix spans structurally or by position, not by comparing their foreground to a hardcoded `MUTED` constant.
- [x] Preserve each span's foreground and modifiers through truncation, aligned table-cell patching, and continuation-line wrapping.

### Focused tests

- [x] Distinct `markdown_marker`, `markdown_heading`, `markdown_link`, `markdown_inline_code`, `markdown_rule`, `markdown_table_border`, and `markdown_table_header` values appear in rendered spans.
- [x] Fenced code still uses `syntax_*` colors and syntect bold/italic styles independently of structural Markdown roles.
- [x] Wrapped quotes/lists repeat or indent prefixes correctly without relying on a specific color.
- [x] Narrow tables preserve cell span styles while applying themed borders/header styling.
- [x] Defaults reproduce current intentional typography and approximately preserve current colors through documented fallbacks.

### Patch boundary and rollback

One Markdown-only patch may change the renderer signature and its direct callers, but not schema, runtime state, fullscreen, or other pane styling. Reverting it returns Markdown to syntect-only input while leaving the dormant phase 2 roles harmless.

### Acceptance

- [x] Markdown contains no native visible palette constants outside syntect-to-ratatui value conversion.
- [x] A custom theme independently controls all seven structural roles.
- [x] Wrapping, truncation, tables, and fenced code preserve per-span style.

---

## 6. Centralize and harden color conversion

**Target:** Named/RGB conversion has one policy, and unsupported ratatui variants never become invented visible gray in syntect or terminal OSC output.

**Depends on:** Phase 1 for background transition semantics and phase 2 for the accepted color contract. Independent of renderer migrations.

### Policy

- Public theme input remains named colors plus six-digit RGB, with or without `#`.
- Maintain one exhaustive named-ANSI-to-RGB table in the TUI color module for conversions that require RGB. Both syntect and OSC helpers consume it.
- `Color::Rgb` passes through. `Color::Indexed` and `Color::Reset` return a typed unsupported/reset result; neither maps to `(0xD4, 0xD4, 0xD4)`.
- For syntect, unsupported variants are rejected during theme construction/application, which must leave prior state unchanged. `Reset` is not a syntax color.
- For terminal background output, `Some(named/RGB)` emits OSC 11 with RGB, `None` or explicit reset intent emits OSC 111, and unsupported indexed input emits no transition plus a warning/error at the validation boundary.

### Work

- [x] Move duplicate named mappings from `theme.rs` and terminal background code behind one `Color -> RGB` helper with explicit error variants.
- [x] Make syntect rebuild fallible before committing effective theme state, so an invalid conversion cannot partially update fields or the cached code theme.
- [x] Isolate OSC sequence selection in a pure helper; keep terminal I/O at the existing app/render boundary.
- [x] Deduplicate settings/runtime parsing without broadening it to ratatui indexed/reset values.
- [x] Keep conversion-table RGB constants documented as backend approximations and exempt from the visible-theme audit.

### Primary files

- `tui/src/ui/color.rs`
- `tui/src/ui/theme.rs`
- `tui/src/ui/render/mod.rs`
- `tui/src/ui/app/mod.rs`

### Focused tests

- [x] Every named color and RGB value produces the same RGB result for syntect and OSC conversion.
- [x] Indexed and reset values never produce fixed gray.
- [x] RGB/named → different RGB/named, color → absent, absent → color, and absent → absent choose the exact OSC 11/111/no-op result.
- [x] Rejected syntax/background updates leave configured, override, effective, syntect, and last-applied terminal state unchanged.

### Patch boundary and rollback

Land as a conversion/internal-error-handling patch with no role additions or renderer migration. Preserve the old public parser surface, making rollback limited to helper/error plumbing.

### Acceptance

- [x] Syntect and OSC use one named-color conversion policy.
- [x] No unsupported color silently degrades to visible gray.
- [x] Background transitions are deterministic and testable without a terminal.

---

## 7. Add cross-path propagation and regression coverage

**Target:** Integration tests prove that configured base, runtime overrides, and explicit theme propagation reach every renderer and entry path.

**Depends on:** Phases 1–6. Focused tests belong in their owning phase; this phase adds only cross-cutting coverage.

### Work

- [x] Provide one test helper that builds a deliberately unusual theme with distinct foreground/background/status/Markdown/stats/syntax values. Keep it test-only and registry-complete.
- [x] Exercise equivalent local in-process and remote snapshot flows: configured snapshot, runtime update, reset, reconnect, and settings reload must end in equal effective themes.
- [x] Render representative jobs/processes/queue rows, extension pane content, thinking, transcript footer, messages, prompt/autocomplete, picker/setup/catalog/stats, process view, and Markdown with the unusual theme.
- [x] Assert semantic styles on selected spans/lines and pure renderer outputs; avoid fragile whole-screen snapshots unless needed for edge-to-edge background behavior.
- [x] Assert pre-app configured fallback separately from in-app effective-theme propagation.
- [x] Add a source scan only if it can scope native render code and maintain an explicit allowlist for the exemptions below without false positives (manual scoped audit performed; no brittle test scan added).

### Required exemptions for any hardcoded-color audit

- Built-in values in `Palette::default`/`Theme::default`; these define defaults rather than bypassing the theme.
- The centralized named-ANSI conversion table and syntect-to-ratatui RGB transfer.
- Deliberate content-derived colors, including valid extension-authored `PaneContent` spans.
- Test fixtures that intentionally identify colors.
- Terminal reset/control behavior that is not a visible semantic color.

Everything else in native renderers is presumed to be a semantic bypass and must migrate or gain a narrow code comment explaining why it is content data rather than theme policy.

### Patch boundary and rollback

Tests and test helpers only. Do not refactor production code merely to satisfy a broad scan; fix a verified bypass in the renderer phase that owns it. The entire patch can be reverted without behavior change.

### Acceptance

- [x] Coverage tests fail if a representative renderer falls back to built-in colors instead of the supplied theme.
- [x] Local/remote reconnect and incremental paths are behaviorally equivalent.
- [x] Exemptions are narrow, documented, and do not hide native renderer constants.

---

## 8. Final audit, validation, and cleanup

**Target:** Finish with one resolved styling path, no dead palette code, and a diff that can be rolled back phase by phase.

**Depends on:** All prior phases.

### Work

- [x] Inspect `git status` and preserve unrelated changes before cleanup.
- [x] Search native TUI render paths for `Color::*`, `Rgb`, `Indexed`, and local palette constants; classify each occurrence as migrated semantic styling or one of phase 7's explicit exemptions.
- [x] Delete dead `tab_active` code, obsolete setters, duplicate role lists, independent fullscreen palettes, duplicate conversion helpers, and stale compatibility code only when no current caller/schema uses them.
- [x] Verify no runtime override is serialized into configured settings and no renderer reads config or daemon view state directly.
- [x] Run focused tests after each implementation patch, then the full gates below.
- [x] Review the aggregate diff by phase for accidental layout, behavior, protocol, or unrelated formatting changes.
- [ ] Manually smoke-test one deliberately unusual theme through startup setup/catalog, local TUI, `bone serve` remote TUI, a runtime override/reset, Markdown/code, extension panes, stats, and process view.

### Required validation

```text
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Use package name `bone` for focused TUI commands, for example `cargo test -p bone <test-filter>`; do not refer to a nonexistent `bone-tui` package.

### Rollback gate

Before merge, verify the implementation history preserves these reviewable boundaries:

1. runtime layering/protocol snapshot separation;
2. role registry/schema/docs cleanup;
3. main-app renderer migrations;
4. fullscreen/pre-app plumbing and migrations;
5. Markdown migration;
6. conversion hardening;
7. cross-path tests and final cleanup.

A regression in one renderer family must be revertible without reverting runtime semantics or public schema. If implementation order requires temporary dormant roles or theme parameters, keep them compiling and tested between patches rather than squashing boundaries together.

### Acceptance

- [x] Every native visible color comes from the effective `Theme`, with only the documented exemptions remaining.
- [x] Configured base plus sparse overrides yields equivalent local, remote, reconnect, fullscreen, Markdown, syntax, extension-pane, and terminal-background behavior.
- [x] Validation, runtime updates, docs, and renderer mappings agree on the exact role registry.
- [x] All required commands pass, or any pre-existing failure is recorded with evidence before merge.
- [x] No unrelated working-tree change is lost.

---

## Recommended implementation order

1. Resolve phase 0 product decisions and freeze the role inventory.
2. Land phase 1 runtime base/override separation and protocol tests.
3. Land phase 2's canonical registry, schema cleanup, validation, and docs without renderer migrations.
4. Migrate main-app renderers in small phase 3 patches.
5. Land pre-app theme resolution, then migrate fullscreen surfaces in phase 4.
6. Migrate Markdown independently in phase 5.
7. Centralize conversion behavior in phase 6.
8. Add cross-path tests, audit exemptions, clean dead code, and run phase 8 gates.

Do not combine runtime layering, public schema changes, and renderer migrations in one patch. At every boundary, the workspace must compile and focused tests for the changed contract must pass.

## Working-tree safety

The working tree was clean before this document rewrite; the expected planning change is only `TUI_THEME_REVAMP_PLAN.md`. Before every implementation phase, inspect `git status --short` and relevant diffs, record newly present unrelated changes, and avoid resetting, replacing, or broadly reformatting them.