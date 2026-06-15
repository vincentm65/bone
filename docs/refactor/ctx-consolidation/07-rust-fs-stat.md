# Step 07 — Replace 4 fs helpers with `ctx.fs.stat` (Rust)

**Goal:** One `ctx.fs.stat(path)` → table `{path, kind, len, readonly}` or **nil** when the
path doesn't exist. Drop `exists`, `is_file`, `is_dir`, `metadata`.

**File:** `src/ext/ctx.rs`

## Changes

1. In the `fs_table` block (ctx.rs:177-243), delete:
   - `fs.exists` (ctx.rs:180)
   - `fs.is_file` (ctx.rs:184)
   - `fs.is_dir` (ctx.rs:188)
   - `fs.metadata` (ctx.rs:223)

2. Add `fs.stat`: like the old `metadata` (ctx.rs:223-241) but **nil on missing** instead of
   erroring. Use `std::fs::metadata(&path).ok()`; return `Value::Nil` when `None`, else the
   `{path, kind, len, readonly}` table. (Only return an error for genuinely unexpected I/O if
   desired; `.ok()` collapsing to nil is acceptable and simplest.)

3. Keep `fs.read_dir` (ctx.rs:192) unchanged.

## Lua migration idioms (used in Step 08)

- `exists(p)` → `ctx.fs.stat(p) ~= nil`
- `is_file(p)` → `local s = ctx.fs.stat(p); return s ~= nil and s.kind == "file"`
- `is_dir(p)` → `local s = ctx.fs.stat(p); return s ~= nil and s.kind == "dir"`

## Verify

- `cargo build` succeeds.
- `grep -nE "fs\.(exists|is_file|is_dir|metadata)" src/ext/ctx.rs` → nothing.
- `ctx.fs.stat("/")` returns `{kind="dir", ...}`; `ctx.fs.stat("/no/such")` returns nil.
