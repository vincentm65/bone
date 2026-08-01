# Development and Validation

This document describes the workflow for Bone's core platform and bundled
reference documents. Optional installed extensions own their implementation and
extension-specific documentation.

## Workspace commands

From the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo build --release
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

`core/defaults/AGENTS.md` is the bundled universal index. The focused documents
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

The theme role table in `docs/configuration.md` is generated from the Rust theme
registry. Keep its `BEGIN GENERATED THEME ROLES` and `END GENERATED THEME ROLES`
markers unchanged, update the generator/test target if the document moves, and
run the theme documentation test.

Bundled docs are compiled with `include_str!`; adding or renaming a topic requires
updating the synchronization list and its tests. Verify both missing-file
creation and stale-file replacement in a temporary config directory. Never claim
materialized docs exist until startup synchronization has been implemented and
validated.

## Safety and review

Use dedicated file tools for text contents, read before editing, and use shell
only for commands or operations the file tools cannot express. Treat approval
and command-policy behavior as part of the public contract. Validate path,
process, and protocol inputs at their boundaries; avoid logging credentials or
unbounded output. A review should report only verified correctness, security,
regression, crash, or dead-code issues.
