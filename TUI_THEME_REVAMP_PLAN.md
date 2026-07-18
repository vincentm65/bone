# TUI theme revamp plan

## Goal

Make every user-visible TUI surface use one coherent semantic theme, preserve configured values when runtime overrides are cleared, and remove schema/runtime behavior that appears supported but is not implemented.

## Principles

- Keep `Theme` as the single resolved TUI styling model.
- Use semantic roles, not component-specific raw colors.
- Keep terminal/backend RGB conversion constants separate from visible styling concerns.
- Preserve current defaults unless a change is intentional and covered by a rendering test.
- Resolve configuration once, then layer temporary runtime overrides without mutating the configured base.
- Avoid one role per widget when existing roles express the same meaning.
- Remove unsupported configuration instead of silently accepting it.
- Preserve unrelated working-tree changes while implementing this plan.

## Status

- [ ] 0. Confirm scope and product decisions
- [ ] 1. Fix configured theme and runtime override semantics
- [ ] 2. Complete theme propagation through main-app surfaces
- [ ] 3. Theme fullscreen and pre-app surfaces
- [ ] 4. Add semantic Markdown styling
- [ ] 5. Align schema, parser, documentation, and runtime behavior
- [ ] 6. Harden terminal color handling
- [ ] 7. Add propagation and regression coverage
- [ ] 8. Run final validation and remove dead styling code

---

## 0. Confirm scope and product decisions

**Target:** Settle choices that affect the shape of the implementation before adding roles or changing APIs.

### Decisions

- [ ] Decide whether `/stats` remains. If the simplification plan removes it, delete it rather than investing in theme propagation.
- [ ] Decide whether `bold`, `italic`, and `underline` remain in `ThemeStyleSpec`:
  - implement them end to end, including storage in resolved roles and rendering; or
  - remove them from the schema and documentation.
- [ ] Decide whether `tab_active` has a real rendering use. Wire it to that surface or remove it.
- [ ] Choose the pre-app theme source for setup and catalog:
  - preferred: resolve the configured theme before entering fullscreen UI;
  - fallback: use `Theme::default()` only when configuration cannot be loaded.
- [ ] Define the indexed-color policy for OSC 11 terminal backgrounds. Do not substitute a fixed gray for arbitrary indexed colors.

### Acceptance

- [ ] Each decision is reflected in the following phases; no placeholder role or compatibility layer remains.
- [ ] Work does not conflict with planned feature deletion in `SIMPLIFICATION_PLAN.md`.

---

## 1. Fix configured theme and runtime override semantics

**Target:** Clearing a temporary runtime override restores the active configured theme, not `Theme::default()`.

### Design

Maintain two distinct inputs:

1. **Configured base theme** — rebuilt from the latest frontend settings snapshot.
2. **Runtime overrides** — sparse highlight overrides received through `ViewDiff::SetHighlight`.

The effective renderer theme is derived by applying runtime overrides over the configured base. A `None` runtime value removes the override and exposes the configured value beneath it. Theme reloads rebuild the base and then reapply remaining overrides.

Keep ownership authoritative:

- The daemon owns the sparse runtime override map in `ViewModel.highlights`.
- Frontend snapshots contain the resolved configured theme plus current runtime overrides.
- The TUI must retain enough base state to remove a live override correctly between snapshots.

### Work

- [ ] Refactor `tui/src/ui/theme.rs` so runtime reset does not consult `Theme::default()`.
- [ ] Make snapshot application establish or replace the configured base before applying overrides.
- [ ] Ensure clearing `bg` restores the configured background and reapplies the correct OSC background state.
- [ ] Ensure clearing optional roles restores `None` when that is the configured value.
- [ ] Preserve syntect rebuilds when effective `syntax_*` values change, but avoid rebuilding when the effective value is unchanged.
- [ ] Verify theme reload behavior while runtime overrides are active.
- [ ] Clarify comments and API documentation: `nil` removes the runtime override; it does not mean “built-in default.”

### Primary files

- `tui/src/ui/theme.rs`
- `tui/src/ui/app/mod.rs`
- `core/src/runtime/view.rs`
- `core/src/ext/types.rs`
- `core/src/ext/api_ui.rs`

### Tests

- [ ] Configured foreground → runtime override → reset restores configured foreground.
- [ ] Configured background → runtime override → reset restores configured background and terminal state.
- [ ] Configured absent background → runtime override → reset issues terminal background reset.
- [ ] Theme reload under an active override changes the hidden base and exposes the new value after reset.
- [ ] Runtime syntax reset restores configured syntax and rebuilds the code theme.

### Acceptance

- [ ] No reset path restores a built-in value when a configured value exists.
- [ ] Snapshot and incremental-diff paths produce the same effective theme.
- [ ] Runtime overrides remain temporary and do not overwrite persisted theme configuration.

---

## 2. Complete theme propagation through main-app surfaces

**Target:** Remove visible hardcoded colors from panes and transcript-adjacent rendering that already run under the main app.

### Work

- [ ] Pass `&Theme` to jobs pane rendering and map status, text, border, and selection styling to semantic roles.
- [ ] Pass `&Theme` to queue pane rendering and remove hardcoded white/gray/selected backgrounds.
- [ ] Pass `&Theme` to processes pane rendering and replace white/yellow/gray/dark-gray styles.
- [ ] Use theme roles for the thinking pane title and body.
- [ ] Use theme roles for transcript-view footer text.
- [ ] Replace hardcoded tool-name and shell-label colors in message rendering.
- [ ] Replace hardcoded prompt-option and autocomplete colors in the bottom pane without disturbing unrelated behavior changes there.
- [ ] Reuse `fg`, `muted`, `subtle`, `border`, `accent`, `good`, `warn`, `error`, and `selection` where their semantics fit.
- [ ] Add a new role only where no existing semantic role accurately represents the state.

### Primary files

- `tui/src/ui/jobs_pane.rs`
- `tui/src/ui/queue_pane.rs`
- `tui/src/ui/processes_pane.rs`
- `tui/src/ui/app/stream/mod.rs`
- `tui/src/ui/transcript_view.rs`
- `tui/src/ui/render/messages.rs`
- `tui/src/ui/render/bottom_pane.rs`

### Acceptance

- [ ] No user-visible `Color::*` or raw RGB/indexed value remains in these renderers unless it is documented as an intentional non-theme terminal/backend operation.
- [ ] Selection, warning, error, success, muted, and normal text are visually controlled by semantic theme roles.
- [ ] Existing bottom-pane changes and tests are preserved rather than overwritten.

---

## 3. Theme fullscreen and pre-app surfaces

**Target:** Picker, setup, catalog, and any retained stats UI receive the same resolved theme as the main application.

### Design

- Make fullscreen lifecycle code responsible only for terminal setup and teardown.
- Pass a resolved `&Theme` into fullscreen renderers instead of maintaining independent palette constants.
- For flows that run before the app is constructed, resolve the configured theme at the call boundary and pass it in explicitly.

### Work

- [ ] Update picker APIs to accept `&Theme`; remove the independent indexed picker palette.
- [ ] Update setup APIs and call sites to accept a resolved theme.
- [ ] Update catalog APIs and call sites to accept a resolved theme.
- [ ] If stats is retained, define semantic chart roles and pass `&Theme` into it; do not directly preserve its 15 hardcoded greens.
- [ ] If stats is removed, delete its palette with the feature and skip stats-specific theme roles.
- [ ] Keep `fullscreen.rs` free of theme policy.
- [ ] Verify both startup-time and in-app setup/catalog entry paths use the expected theme.

### Primary files

- `tui/src/ui/picker.rs`
- `tui/src/ui/setup.rs`
- `tui/src/ui/catalog.rs`
- `tui/src/ui/stats.rs` if retained
- `tui/src/ui/fullscreen.rs`
- relevant command and startup call sites

### Acceptance

- [ ] Fullscreen surfaces visibly respond to a deliberately unusual configured theme.
- [ ] No duplicate picker/fullscreen palette remains.
- [ ] Pre-app fallback behavior is explicit and tested.

---

## 4. Add semantic Markdown styling

**Target:** Structural prose and fenced code both derive styling from the active theme.

### Roles

Prefer a compact semantic set rather than mirroring every Markdown token:

- `markdown_marker` — quote markers and list prefixes
- `markdown_heading` — heading text
- `markdown_link` — link labels/URLs as appropriate
- `markdown_inline_code` — inline code
- `markdown_rule` — horizontal rules
- `markdown_table_border` — table separators
- `markdown_table_header` — table headers

Reuse existing `fg`, `muted`, and `subtle` roles where a distinct Markdown role provides no useful control. Keep fenced-code tokens on the existing syntax theme.

### Work

- [ ] Add only the approved semantic roles to `Theme` and their default derivation.
- [ ] Add structured settings/highlight names for those roles using the existing precedence rules.
- [ ] Replace structural hardcoded colors in `render/markdown.rs`.
- [ ] Preserve fenced-code syntect foreground and font-style behavior.
- [ ] Ensure line wrapping retains per-span Markdown and syntax styles.
- [ ] Document each new role and its fallback.

### Acceptance

- [ ] Markdown structural colors contain no raw visible palette constants.
- [ ] Existing themes remain visually stable through derived defaults.
- [ ] A custom theme can visibly control headings, links, inline code, rules, and tables.

---

## 5. Align schema, parser, documentation, and runtime behavior

**Target:** Every accepted theme setting has defined behavior, and every supported runtime role is discoverable and validated.

### Work

- [ ] Implement or remove `ThemeStyleSpec` modifiers according to phase 0.
- [ ] Wire or remove `tab_active` according to phase 0.
- [ ] Decide whether `input_bg`, `input_prefix`, and `input_cursor` become typed fields or remain documented free-form highlights; avoid two inconsistent configuration paths.
- [ ] Validate configured color strings during settings validation rather than warning only during rendering.
- [ ] Validate known highlight names and color references while preserving a clear extension policy.
- [ ] Keep legacy flat-field precedence documented while supported; remove legacy fields if compatibility is no longer required.
- [ ] Add direct `parse_color()` tests for every accepted named and hex form and every rejected form.
- [ ] Update generated/default documentation to match actual schema, precedence, reset semantics, and accepted color syntax.

### Primary files

- `core/src/config/settings.rs`
- `core/defaults/AGENTS.md`
- `tui/src/ui/color.rs`
- `tui/src/ui/theme.rs`
- theme-related tests

### Acceptance

- [ ] No schema field is silently ignored.
- [ ] Invalid persisted colors fail validation with a field-specific message.
- [ ] Documentation and runtime agree on role names, modifiers, formats, precedence, and reset semantics.

---

## 6. Harden terminal color handling

**Target:** Terminal background application is correct for every background form the theme system accepts.

### Work

- [ ] Keep named-color conversion centralized and exhaustively tested.
- [ ] Remove duplicated named-color mappings where practical, including syntect conversion.
- [ ] For indexed colors, either resolve the terminal index through a documented xterm palette or reject indexed backgrounds before OSC conversion.
- [ ] Treat `Color::Reset` as a terminal reset operation, never as fixed gray.
- [ ] Verify switching among RGB, named, indexed/unsupported, and absent backgrounds emits the correct OSC 11/111 sequence.
- [ ] Keep terminal conversion constants out of the visible hardcoded-color audit.

### Primary files

- `tui/src/ui/render/mod.rs`
- `tui/src/ui/app/mod.rs`
- `tui/src/ui/theme.rs`

### Acceptance

- [ ] No indexed or reset color silently maps to `(0xD4, 0xD4, 0xD4)`.
- [ ] Background transitions are deterministic and covered by tests without requiring a real terminal.

---

## 7. Add propagation and regression coverage

**Target:** Tests fail when a renderer bypasses the theme or runtime layering regresses.

### Work

- [ ] Create a deliberately unusual test theme whose roles are distinct and easy to identify.
- [ ] Render jobs, queue, processes, thinking, transcript footer, messages, and bottom-pane states with that theme and assert emitted styles.
- [ ] Cover picker, setup, catalog, and retained stats rendering with the unusual theme.
- [ ] Cover Markdown structural roles independently from fenced-code syntax roles.
- [ ] Cover structured style backgrounds and modifiers if retained.
- [ ] Cover invalid colors and unknown role/reference behavior.
- [ ] Cover named, RGB, indexed, and reset terminal conversion behavior.
- [ ] Add a focused scan/test guard only if it can distinguish visible renderer constants from intentional backend conversions without false positives.

### Acceptance

- [ ] Tests demonstrate complete theme propagation rather than only testing theme object construction.
- [ ] Runtime reset, snapshot reload, and background transition regressions are covered.
- [ ] Assertions target semantic styles and behavior, not fragile full-screen snapshots unless a snapshot adds clear value.

---

## 8. Final validation and cleanup

**Target:** Finish with one coherent implementation and no obsolete palette code.

### Work

- [ ] Search TUI render paths for remaining `Color::*`, raw RGB/indexed colors, and local palette constants; classify every remaining occurrence.
- [ ] Delete dead roles, duplicate conversion helpers, independent palettes, and compatibility code made obsolete by the revamp.
- [ ] Run formatting.
- [ ] Run focused theme and renderer tests.
- [ ] Run the complete workspace test suite.
- [ ] Run workspace clippy with warnings treated according to project policy.
- [ ] Review the final diff for accidental changes to unrelated work.
- [ ] Confirm startup, local TUI, and remote TUI theme behavior remain equivalent.

### Required validation

```text
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Add focused commands for changed renderer integration tests while implementing each phase rather than waiting for the final pass.

### Acceptance

- [ ] Every remaining visible color is sourced from `Theme` or explicitly justified in code.
- [ ] Local, remote, fullscreen, Markdown, syntax, and terminal-background paths use the resolved theme consistently.
- [ ] No accepted configuration is ignored.
- [ ] No pre-existing unrelated working-tree change is lost.

---

## Recommended implementation order

1. Make the phase 0 decisions.
2. Fix runtime base/override layering and its tests before changing renderers.
3. Migrate main-app surfaces with the existing semantic roles.
4. Resolve fullscreen theme loading and migrate picker/setup/catalog.
5. Add the smallest useful Markdown role set and migrate Markdown.
6. Align schema and documentation after the final role set is known.
7. Harden terminal conversion.
8. Add broad unusual-theme coverage, scan for bypasses, and run final validation.

Keep each phase reviewable. Avoid combining runtime-state changes, schema expansion, and all renderer migrations in one patch.

## Working-tree safety

At plan creation time, these files already contain unrelated modifications and must be preserved:

- `tui/src/ui/app/stream/mod.rs`
- `tui/src/ui/render/bottom_pane.rs`
- `tui/tests/bottom_pane_test.rs`

Before each implementation phase, inspect `git status` and the relevant diffs. Do not reset, replace, or reformat unrelated changes.