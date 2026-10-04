# Bone 3 catalog

This directory is the native Bone 3 catalog. Every package is a normal Bone 3
plugin and may contain a `core.lua` half, a `tui.lua` half, shared `lua/`
modules, colors, documentation, and tests. Packages are installed by copying
their directory to `~/.bone/plugins/<name>`; the runtime's `/plugin` command
then controls loading and unloading.

The catalog deliberately keeps distribution separate from execution. `catalog.json`
is generated from package manifests and records the files and hashes an
installer should verify before replacing an installed package.

The first ports preserve the old catalog's names and user-facing commands,
while using Bone 3's APIs (`bone.tool.register`, `bone.cmd.create`,
`bone.hook`, `bone.ask`, `bone.ui.panel`, and `bone.model.complete`). A package
with no core or TUI half is still valid; for example, a colors-only package
only contributes files under `colors/`.

Generate the index with:

```sh
python3 catalog/gen-index.py
```

Install one package into a Bone 3 config directory after generating the index:

```sh
python3 catalog/install.py web_search
```

Use `--force` to replace an existing package. Installation is staged and the
package files are hash-checked against `catalog.json` before they are copied.

Run the package syntax checks with:

```sh
python3 catalog/check.py
```

After `cargo build -p bone`, load every catalog core half through a real
headless server:

```sh
python3 catalog/runtime-check.py
```
