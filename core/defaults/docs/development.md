# Development and Validation

This document describes the workflow for Bone's core platform and bundled
reference documents. Optional installed extensions own their implementation and
extension-specific documentation.

## Workspace commands

From the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo build --release                    # default protocol/core/TUI members
cargo build --release -p bone-desktop     # native app
cargo test -p bone-client
cargo build --release --workspace        # all Rust workspace packages
```

Use focused checks while iterating, for example:

```sh
cargo test -p bone config::
cargo test -p bone config::theme::tests
cargo test -p bone --test <name>
```

Run the smallest relevant test first, then formatting, package tests, and the
full workspace suite before reporting a change complete. If an environment or
pre-existing failure prevents a check, report the exact command and failure.

## Change workflow

1. Read the relevant topic document and the source callers/tests before editing.
2. Keep the daemon/core as the authority; extend existing protocol and runtime
   paths instead of adding frontend-only behavior.
3. Update tests with behavior changes, including cancellation, reconnect,
   approval, and multi-client cases when applicable.
4. Run formatting and focused tests, then the workspace suite.
5. Review the diff for unrelated changes, generated content, secrets, and stale
   documentation.

Preserve unrelated working-tree changes. Do not commit or push unless explicitly
requested.

## Documentation ownership

`core/defaults/AGENTS.md` is the bundled self-modification guide and index; the
system prompt gives agents its absolute path. The focused documents
under `core/defaults/docs/` are Bone-owned core-platform references and are
materialized under the resolved config directory at startup. Startup
synchronization forcibly replaces stale bundled reference files so the running
build and its reference stay consistent; it does not overwrite user extension
implementation files.

When behavior changes, update the one relevant topic document and keep the index
as an index. Do not copy optional installed features into the core reference.
Extension-owned docs may describe an extension's own commands, tools, or data,
but must not redefine core ownership or protocol contracts.

## Generated and bundled files

The theme role table in `docs/configuration.md` mirrors the Rust theme registry
in `core/src/config/theme.rs`. Keep its `BEGIN GENERATED THEME ROLES` and
`END GENERATED THEME ROLES` markers, and update the table by hand in the same
change whenever a theme role is added, removed, or retyped — no test regenerates
it, so verify the table against `theme.rs` manually.

Bundled docs are compiled with `include_str!`; adding or renaming a topic requires
updating the synchronization list and its tests. Verify both missing-file
creation and stale-file replacement in a temporary config directory. Never claim
materialized docs exist until startup synchronization has been implemented and
validated.

## UI glyph coverage

Desktop UI text is drawn from bundled fonts onto a character grid, so new spinner
frames, box or block drawing, and decorative text are a font-coverage question.
Inspect a candidate face with `fc-query --format=%{charset}` on the bundled file in
`native/assets/fonts/`; `native/src/theme.rs` asserts that every bundled spinner
preset and the core UI glyph set is drawable by the installed chain.

Do not check coverage with egui's `has_glyph`/`has_glyphs`. `epaint` chooses the
replacement face as the first face of a family that covers its own replacement
character, and the bundled monospace fallback covers that character, so every
glyph the fallback provides — exactly the set worth verifying — reports
`has_glyph == false`. Probe advance width instead: after one `Context::run_ui` pass
has resolved the fonts, `FontsView::glyph_width` returns `0.0` for a character no
face provides and a positive value otherwise. Keep a positive control (a Latin
letter must be nonzero) and a negative control (a codepoint the bundle does not
cover, such as `U+10800`, must be zero) in the test so it cannot pass vacuously.

## Safety and review

Use dedicated file tools for text contents, read before editing, and use shell
only for commands or operations the file tools cannot express. Treat approval
and command-policy behavior as part of the public contract. Validate path,
process, and protocol inputs at their boundaries; avoid logging credentials or
unbounded output. A review should report only verified correctness, security,
regression, crash, or dead-code issues.
