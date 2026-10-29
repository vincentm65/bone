# Catalog plugin styling alignment

## Goal

Bring catalog plugins that draw UI into the same rendering approach now used by
`todo` and `git-diff`:

- Keep the data, interaction state, and semantic states shared between
  frontends.
- Keep TUI and GUI renderers separate where their primitives differ.
- Use `bone.nodes.widgets` and `bone.nodes.style` for GUI controls, spacing,
  sizes, radii, and common surfaces.
- Use semantic highlight groups instead of plugin-specific colors or copied
  palette values.
- Let the active colorscheme and GUI library control the common look.
- Preserve plugin-specific content and behavior without reimplementing the
  standard widget chrome.

This is a styling and renderer audit. It does not require making every TUI
line renderer into a GUI node renderer, and it does not require replacing the
native GUI implementations of the terminal or toolstrip.

## Reference pattern

`todo` and `git-diff` are the current reference points:

- The TUI returns line/span data with semantic highlight names.
- The GUI returns a node tree or uses a standard widget where one exists.
- Shared state and interaction behavior remain in the plugin.
- GUI-specific layout and pointer behavior live in the GUI rendering path.
- Default GUI styling is used instead of independently reproducing cards,
  buttons, checklists, borders, and spacing.

Relevant APIs and documentation:

- `bone.nodes.widgets`
- `bone.nodes.style`
- `bone.nodes.popups`
- `docs/customizing/customizing-gui.md`, especially the sections on styles,
  widgets, panels, popups, and chat views.

## Priority summary

| Priority | Plugins | Main problem |
| --- | --- | --- |
| High | `fork`, `goal`, `mcp`, `md`, `subagent` | Custom GUI nodes contain substantial hardcoded chrome and do not consistently use the shared style/widget system. |
| Medium | `ask_user`, `context`, `usage` | Already use some default style infrastructure, but still mix it with plugin-owned geometry, controls, or highlight definitions. |
| Review only | `windows` | Uses the style system, but its custom layout, title bars, split handles, and drag/drop affordances are core behavior of the plugin. |
| No GUI migration | `term`, `toolstrip`, `prompt-suggestions` | The GUI rendering is native or only decorates a native prompt; the TUI renderer is the plugin-owned part. |
| No node-style work | `codex`, `history`, `idle_compact`, `recap`, `review`, `shell`, `skill`, `themes` | These provide text, notifications, prompt content, colorschemes, or data to built-in UI. |

## Plugins requiring work

### `fork` — high priority

**Current styling:** `plugins/fork/ui.lua` contains both renderers. The TUI
uses `bone.ui.box` and line spans. The GUI branch constructs selectable rows
and buttons directly, with hardcoded padding, radius, and text sizes. It
conditionally uses `bone.nodes.popups` and `bone.nodes.widgets`, but also has a
substantial fallback that reproduces widget styling.

**Work:**

- Use the standard popup/list and button widgets as the primary GUI path.
- Move selection, point/session, and expanded/collapsed state into semantic
  states rather than styling each row ad hoc.
- Use `bone.nodes.style` for sizes, radii, and spacing.
- Keep the TUI line renderer separate, but align its highlight names with the
  GUI states.
- Reduce the custom fallback to a structural fallback rather than a second
  visual system.

### `goal` — high priority

**Current styling:** `plugins/goal/ui.lua` has a TUI `band` renderer and a GUI
`goal_node` renderer. The GUI is a custom one-row node inside a panel and uses
mostly direct node fields and highlight names. It does not use the shared node
style helpers.

**Work:**

- Decide whether the goal is a standard status/band widget or a plugin-owned
  status panel. The former is preferred.
- Use shared panel sizing and style values for the GUI node.
- Keep semantic states for running, completed, failed, and check-error text.
- Ensure the TUI and GUI use the same semantic highlight names even though the
  layout is different.
- Avoid hardcoding dimensions that prevent a GUI library from adapting the
  status band.

### `mcp` — high priority

**Current styling:** `plugins/mcp/ui.lua` contains both a TUI line/box
renderer and a GUI node tree. The GUI has custom server rows, tool lists,
fields, confirmation bars, and buttons. It uses widgets and popup helpers when
available, but also hardcodes spacing, radii, text sizes, and button surfaces.

**Work:**

- Build the GUI from the standard popup, list, field, button, surface, and
  title/note widgets.
- Use `bone.nodes.style` for sizes, radii, and spacing.
- Keep server state semantic (`connected`, `disabled`, `failed`, `signing`,
  etc.) and map it to highlight groups in one place.
- Use the same widget conventions for add, reconnect, sign-in, enable/disable,
  remove, and cancel actions.
- Keep the TUI renderer but remove any copied palette values in favor of
  semantic groups.

### `md` — high priority

**Current styling:** `plugins/md/lua/md_browser.lua` contains both the TUI
popup and the GUI two-pane browser. The GUI owns the file list, tabs, filter
surface, buttons, scrolling regions, and preview container. Markdown content
can use the desktop Markdown renderer, but the browser chrome is custom.

**Work:**

- Use standard popup chrome, tabs/segmented controls, list entries, buttons,
  fields, and surfaces.
- Use `bone.nodes.style` rather than repeating padding, radius, and font-size
  values.
- Keep the desktop Markdown node renderer for document content.
- Preserve the shared selection and scroll state across TUI and GUI.
- Make loading, empty, error, and selected states semantic and consistent.

### `subagent` — high priority

**Current styling:** `plugins/subagent/ui.lua` contains both renderers. The GUI
creates its own agent list, detail fields, edit bar, confirmation row, and
buttons. It conditionally uses list and button widgets but has a large custom
fallback and hardcoded sizes, padding, and radii.

**Work:**

- Use standard list, field, card/surface, button, and confirmation widgets.
- Use `bone.nodes.style` for typography, spacing, and radii.
- Keep editing, validation, save errors, and inheritance as shared state;
  only the presentation should differ between frontends.
- Replace ad hoc selected/editing/empty styles with semantic widget states.
- Keep the TUI renderer readable and map its highlight names to the GUI states.

## Plugins needing a partial alignment pass

### `ask_user`

**Current state:** The GUI already reads `bone.nodes.style` for sizes and
radii, and it has a proper node-based modal. It still owns much of the modal
layout, input surface, option rows, button surfaces, and highlight selection.
The TUI separately derives a prompt-like style and draws its own border and
region.

**Work:**

- Prefer standard dialog, option, input, and button widget conventions where
  they exist.
- Keep the question-specific behavior and accessibility roles.
- Replace duplicated GUI fallback chrome with shared widgets.
- Keep the TUI prompt-derived appearance, but ensure its semantic groups are
  compatible with the GUI groups.
- Audit `InputBackground`, `Selection`, `Accent`, and related groups so a
  colorscheme controls both frontends predictably.

### `context`

**Current state:** The TUI defines and updates `Context*` highlight groups and
renders its own dashboard lines. `plugins/context/lua/context/nodes.lua`
builds a custom GUI dashboard with tabs, buttons, sections, meters, and
scrolling. It uses default sizes/widgets in places but still owns the overall
visual language.

**Work:**

- Retain `Context*` as semantic groups; do not replace them with literal
  colors.
- Use standard tabs, buttons, cards, dividers, and scrolling surfaces.
- Centralize the mapping from context category to semantic highlight group so
  TUI and GUI cannot drift.
- Use shared style values for section spacing, typography, and borders.

### `usage`

**Current state:** The TUI in `plugins/usage/ui.lua` owns charts, heatmaps,
value styles, and dashboard line spans. The GUI in
`plugins/usage/lua/usage/nodes.lua` already uses `bone.nodes.style` for sizes
and some widgets, but cards, chips, borders, padding, heatmap cells, and
highlight groups remain custom.

**Work:**

- Use standard cards/surfaces, segmented controls, buttons, and dividers.
- Keep usage-specific semantic groups such as heat levels and values, but
  derive their defaults from the colorscheme rather than fixed palette values.
- Use shared size/radius/spacing values throughout the GUI dashboard.
- Verify that charts remain legible in light and dark colorschemes.
- Keep the TUI chart layout independent while sharing semantic color names.

## `windows`: review, not a generic conversion

`windows` already requires `bone.nodes.style` in `gui.lua` and uses it for
hover colors, alpha, sizes, radii, separators, title bars, and drop previews.
Its custom node tree is not accidental chrome: split handles, drag/drop zones,
window bars, focus indicators, and panel embedding are the plugin's feature.

The work should therefore be limited to:

- Audit hardcoded values against `bone.nodes.style`.
- Use shared widgets for close buttons, spinners, and standard affordances.
- Keep window-specific semantic groups such as active/inactive bars and
  separators.
- Verify that embedded panels (`term`, `todo`, `git-diff`, and other node
  panels) inherit the same default GUI surface and spacing.
- Do not flatten the window manager into a generic popup layout.

## Plugins that should not be migrated to custom GUI nodes

### `term`

The TUI renderer is plugin-owned in
`plugins/term/lua/bone/ui/terminal.lua`. The desktop terminal is provided by
Bone's native GUI implementation; `plugins/term/gui.lua` intentionally does
not replace it. The `windows` plugin may wrap terminal opening and place the
panel in its tiled layout.

Only audit semantic terminal highlight handling and panel inheritance. There is
no separate catalog GUI renderer to convert.

### `toolstrip`

The TUI renderer and its highlight defaults are in `plugins/toolstrip/tui.lua`
and `plugins/toolstrip/ui.lua`. The GUI plugin file only enables options; the
actual GUI toolstrip is Bone's native renderer.

Only keep the shared semantic highlight names aligned. Do not duplicate the
native GUI toolstrip in the catalog plugin.

### `prompt-suggestions`

Both `tui.lua` and `gui.lua` decorate the native prompt/composer with a
placeholder and ghost text using the `Placeholder` group. It does not create a
GUI node or widget.

Only verify that the `Placeholder` group remains part of the shared prompt
style contract.

## Text-level and built-in UI plugins

These do not need custom GUI node work for this effort:

- `codex` — supplies highlighted lines to `bone.ui.pager`.
- `history` — supplies items to `bone.ui.select`.
- `idle_compact` — uses `bone.notify`.
- `recap` — supplies styled lines to the chat view.
- `review` — sets prompt text and emits notifications.
- `shell` — adds and updates chat text items.
- `skill` — supplies pager lines and prompt text.
- `themes` — invokes the built-in selector and applies colorscheme files.

These should only be checked for semantic highlight names if a shared highlight
is renamed or removed. They should not grow custom GUI renderers solely to
match the `todo`/`git-diff` implementation.

## Cross-cutting implementation work

1. **Define the GUI style contract.** Document which common controls should
   come from `bone.nodes.widgets` and which values should come from
   `bone.nodes.style`.
2. **Standardize semantic groups.** Keep state names stable across TUI and GUI;
   do not encode colors in plugin renderers.
3. **Separate state from presentation.** Shared code should calculate rows,
   selection, loading, error, and action state. TUI and GUI code should only
   lay those states out.
4. **Reduce duplicate fallbacks.** A fallback should preserve structure and
   behavior, not maintain a second complete visual design.
5. **Check colorscheme behavior.** Test at least the default, a dark scheme,
   and a light scheme. Check selected, hover, disabled, error, active, and
   completed states.
6. **Check library behavior.** Test the standard GUI library and a minimal/bare
   library where the optional widget modules may be absent.
7. **Test both frontends.** Exercise keyboard navigation in the TUI and pointer,
   focus, scrolling, and accessibility roles in the GUI.
8. **Update plugin documentation.** If a plugin exposes style groups or expects
   a widget capability, document that contract in its README.

## Suggested order

1. `fork`, `mcp`, `md`, and `subagent` — the largest duplicated GUI chrome.
2. `goal` — small surface and a good panel/widget test case.
3. `ask_user` — modal, input, focus, and accessibility behavior.
4. `context` and `usage` — dashboards with custom semantic colors and charts.
5. `windows` — audit after the panel and widget conventions settle.
6. Run the cross-cutting colorscheme, bare-library, and frontend tests.

`term`, `toolstrip`, `prompt-suggestions`, and the text-level plugins should be
kept out of the main migration unless the shared style contract itself changes.
