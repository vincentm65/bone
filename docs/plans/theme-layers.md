# Layered highlight groups

## Purpose

Loading a colorscheme used to erase UI, plugin and user groups, including
input change counts and todo strikethrough. Links copied their target's
style once, so they also became stale after a switch.

## Design

- Lookup order: overrides → scheme → shared built-in ANSI fallback.
- `bone.hl.set` writes to the scheme while a colorscheme runs; otherwise it
  writes to overrides. Choose the first whole definition, without merging.
- A scheme load swaps in an empty scheme layer and puts the old one back
  if the file fails. A load nested in another adds to the outer layer and
  leaves `colors_name` to the outer one.
- Links follow current definitions. Self-links start at the next layer; a
  group whose links end at a missing group or a cycle draws as `Normal`
  (Default style if `Normal` itself does).
- No resolved cache: each lookup walks the (short) link chain, so setting a
  group, including generated `@term` ones mid-render, costs O(1).
- `bone.hl.reset()` in a scheme file clears the scheme layer; elsewhere both.
  Config reload creates a fresh theme.
- Generated `@term` overrides link to `Normal` plus explicit cell styling;
  reset removes them and the next terminal draw recreates them.
- `ansi.lua` must not reset highlights. Default setup loads `black` once.

## Limits

Scheme-specific tweaks belong in a scheme extending the base, not in
persistent overrides. Colors copied through `bone.hl.get` remain snapshots
until the Lua code runs again. Per-field links and a separate generated-group
layer are out of scope. Existing runtime copies in user libraries still need
updating separately.

## Verification

Run `cargo test -p bone-tui`. Coverage includes override survival, old scheme
removal, live/self-links, cycles, whole-definition replacement, nested/failed
loads, and generated styles following `Normal`. Manually switch between
`ansi` and `black` to check todo strikethrough, git +/- and session colors.
