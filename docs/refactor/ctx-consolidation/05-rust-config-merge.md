# Step 05 — Merge `config.get_table` into `config.get` (Rust)

**Goal:** One `ctx.config.get(section, key?)`. With `key` → the value (current `get`
behavior). Without `key` → the whole section table (current `get_table` behavior).

**File:** `src/ext/ctx.rs`

## Changes

1. Change `config_get_fn` (ctx.rs:860) signature to `(section, Option<String> key)`.

2. If `key` is `None`: read+parse the YAML (same as `get_table`, ctx.rs:905-917) and return
   `yaml_to_lua(lua, &doc)`.

3. If `key` is `Some`: keep the existing fields-array-then-top-level lookup (ctx.rs:874-899).

4. Factor the read+parse (read file → `serde_yaml::from_str` → handle missing file → nil)
   into a small local closure/helper to avoid duplicating between the two branches.

5. Delete `config_get_table_fn` (ctx.rs:904-918) and `config_table.set("get_table", ...)`.

6. `ctx.config.dir` (ctx.rs:856) unchanged.

## Verify

- `cargo build` succeeds.
- `grep -n "get_table" src/ext/ctx.rs` → nothing.
- `ctx.config.get("compact", "model")` returns the value.
- `ctx.config.get("compact")` returns the full section table.
- Missing section returns nil in both forms.
