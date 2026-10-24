# Desktop GUI: performance, dead code and structure

A review of `crates/bone-gui` and its Lua runtime (`runtime/gui`,
`runtime/lua/bone/nodes`, `runtime/lua/bone/desktop`), written against the
working tree after the grid was removed (`block.rs`, `rich.rs`, `screen.rs`
deleted, node view in place). Each item has the problem, the evidence, the
proposed change and what it does to the line count. Nothing here is urgent:
the GUI is fast. The point is to stop it from getting slower as chats grow,
and to delete what the grid left behind.

## How it was measured

`crates/bone-gui/tests/perf.rs` (release, headless, so CPU only; GPU time and
idle power are not covered):

```
cargo test -p bone-gui --release --test perf -- --ignored --nocapture
```

| Scenario | ms / frame |
|---|---|
| welcome screen, idle | 0.02 |
| welcome, animation tick (20 fps) | 1.3 |
| chat idle / scroll (24 turns, ~100 items) | 0.06 / 0.2 |
| typing | 0.25 |
| streaming | 0.2 |
| sidebar slide | 0.5 |

The same test with 1000 turns (~4000 items) showed `realize` going from
0.018 to 0.19 ms per frame. That scaling, not the absolute numbers, is the
performance finding. Any change below that touches the frame path should
re-run both sizes (the long one as a temporary copy of `perf.rs` with the
loop bound in `fill` raised).

## Summary

| # | Issue | Kind | Net lines (est.) |
|---|---|---|---|
| 1 | Image textures are never freed; failed loads never retry | leak | +25 |
| 2 | Chat frames cost O(items in chat), not O(items in view) | perf | -20 |
| 3 | Every visible chat item's node tree is deep-cloned every frame | perf | 0 |
| 4 | `prompt_changed()` rebuilds a buffer and JSON on every frame | perf | -10 |
| 5 | Small per-frame allocations (fonts, hits, paths, transcript) | perf | -15 |
| 6 | Config watcher polls the file tree every 500 ms, blocking | idle cost | -40 |
| 7 | A running turn rebuilds the whole tree 10 times a second | idle cost | -3 |
| 8 | Welcome field redraws at 20 fps | none | 0 |
| 9 | Dead code and stale comments | cruft | -60 |
| 10 | The chat cache is used as a "changed?" flag | structure | 0 |
| 11 | Windows and panels are still sized in character cells | structure | 0 to -300 |
| 12 | `nodeview.rs` is 2,773 lines doing eight jobs | structure | 0 |
| 13 | `LuaSide` has ~50 fields, half of them the view's | structure | 0 |
| 14 | The op dispatch chain clones its arguments up to five times | structure | -10 |
| 15 | `window_info()` goes through JSON on every build | perf | -15 |

---

## 1. Image textures are never freed

**Problem.** `Previews.textures` (`preview.rs`) holds a `TextureHandle` for
every image ever drawn, for the life of the process. A texture is up to
2048×2048 RGBA (16 MB), so a long session that scrolls through screenshots
grows GPU memory without bound. A `Failed` entry is also permanent: an image
whose file was briefly missing never loads again until restart.

**Proposed change.** Keep the map, add two rules inside `preview.rs`:

- Each `Ready` entry records `last_used: Instant` (set in `pixels()`) and its
  byte size (`tex.size()` × 4). After a frame, if the total exceeds a budget
  (`const BUDGET: usize = 256 << 20`), drop the least recently used entries
  until under it. Dropping the last `TextureHandle` frees the texture in
  egui, and the next `pixels()` call re-decodes off-thread as it does today.
- `Failed` stores its `Instant` and counts as missing after 5 s.

No other file changes: `pixels()` is the only entry point, and the sweep runs
from `view_ui` after painting (one call).

**Cost.** About +25 lines, none removed. Scrolling back to an evicted image
shows `Loading` for one decode.

## 2. Chat frames are O(items in the chat)

**Problem.** `realize_list` (`nodeview.rs`) calls `chat_items()` every frame
for a chat list, before it knows whether anything is missing. That runs
`Chat::stale()`, which builds `items()` for the whole transcript, computes a
`RenderKey` for every item and looks each up in the cache, then
`check_chat_deps` and `data::generation` over all chats. Measured: 0.018 ms
at ~100 items, 0.19 ms at ~4000, linear. It is paid on frames where nothing
changed and every visible item is already in `ListState.nodes`.

**Proposed change.**

1. In `realize_list`, build `ChatItems` lazily: only when the loop hits an
   index missing from `cached` (or the frame is a rebuild). Frames that are
   pure scroll-within-cache or idle repaint skip it entirely. `st.chat` is
   replaced by `chat.used` only when `chat` was actually built, so the kept
   nodes are not lost.
2. Replace the whole-chat `stale()` for the GUI's use with a per-item
   question on `ChatBuffer` in `bone-ui`:

   ```rust
   /// The key `item` (at 0-based `index` in `items`) would be cached under,
   /// if its cached entry is missing or out of date.
   pub fn stale_key(&self, items: &[Item], index: usize, width: usize,
                    generation: (u64, u64), now: Instant) -> Option<RenderKey>
   ```

   and reimplement `stale()` as a loop over it, so there is one definition of
   freshness (the TUI's `render_chat` keeps calling `stale()`). The GUI then
   asks only about the 10 to 20 items it is about to build.
3. `check_chat_deps` returns early when `data` equals the value it last
   checked for this chat (store it next to the cache), instead of walking
   every cached item's deps each time.

`items()` itself stays an O(N) allocation of 16-byte structs; it is only
called on a miss after this. If it shows up later, memoize it on a structural
revision that `push`/`touch`/Lua-item changes already bump.

**Target.** `realize` at 1000 turns within 2× of its 24-turn figure.
**Cost.** About -20 lines (the `stale` map and `ChatItems.stale` field go;
the per-item function replaces them).

## 3. Visible chat nodes are deep-cloned every frame

**Problem.** `Node` owns its `Vec<Node>` children and strings. In
`realize_list`, `chat.used.insert(*key, item.clone())` deep-clones each
visible chat item's tree every frame so it can be stored in `st.chat`, while
the original goes into `nodes` → `st.nodes`. A long markdown reply is
hundreds of nodes. The profile hides it because the benchmark items are small.

**Proposed change.** Hold items as `Rc<Node>` at the item level only (not the
children, so `node.rs`, `flex.rs` and the painter are untouched):

- `ListState.nodes: HashMap<usize, Rc<Node>>`, `ListState.chat` and
  `ChatItems.{kept,used}` likewise, `Window.nodes: Vec<Rc<Node>>`.
- `make_item` and `chat_item` return `Rc<Node>`; `used.insert(key, Rc::clone(..))`
  is a refcount bump. `flex::place(&n, ..)` takes the deref.

`Node` already holds `mlua::Function` (`List.item`), so it is `!Send`; `Rc` is
consistent with `Sink`, which already uses one.

**Cost.** Net ~0 lines (type changes only). Removes one tree clone per visible
item per frame. Do this together with #2, since both rewrite the same loop.

## 4. `prompt_changed()` does work on every frame

**Problem.** `input_ui` calls `prompt_changed()` each frame. It builds a
`TextBuffer` from the whole draft (`cursor_json` → `buffer()`), makes two JSON
values, clones the draft and the image list into a tuple, and compares that
to `last_prompt`. Almost always nothing changed.

**Proposed change.** Compare before building anything. `last_prompt` becomes
`(String, Vec<ImageAttachment>, usize, usize)` (text, images, anchor,
cursor), compared by reference against `self.draft`, `self.images` and
`self.prompt.{anchor, cursor}`. Only when they differ: update it, build the
cursor/selection JSON, invalidate the view, fire `prompt/changed`. The two
`serde_json::Value` fields in the tuple are derived data and go away.

**Cost.** About -10 lines; zero allocations on an idle frame.

## 5. Small per-frame allocations

Each is microseconds, but they are all on the path every frame runs, and
each is a few lines.

- **`Fonts` copies the draft and every text input's text** (`view_ui`,
  `nodeview.rs:917,924`) only so `Metrics::input/text_input` can count rows.
  Keep a `draft_rows`-style snapshot instead: store `Rc<str>` for the draft
  and for each `InputBuf.text`, refreshed when the text changes (they are
  written in one place each), and let `Fonts` clone the `Rc`s.
- **`wheel()` clones `hits`** (`:1699`). `std::mem::take` it for the loop and
  put it back; `call_handlers` does not touch it.
- **`paint` builds `format!("{path}.{n}")` and a `String` key for every
  node,** most of which have no id and no interaction. Pass the path as a
  `u64` hash chain and build the `@path` string only in the branch that needs
  a key (a node with handlers, focus, tooltip, role, or a list item).
- **`view_state.transcript` is rebuilt every frame** (`:960`) but is read only
  by `Gui::transcript()`, which tests and `Headless` call. Delete the field
  and the per-frame block; compute it in `transcript()` from
  `lists["chat"].nodes` ordered by index.

**Cost.** About -15 lines.

## 6. The config watcher polls forever

**Problem.** `watch_config` spawns a task that, every 500 ms for the life of
the window, walks `lua/`, `runtime/`, `colors/`, the library folder and every
plugin directory, stats each `.lua` file and hashes the result
(`lua::lua_sources`). It does blocking `std::fs` calls on a tokio worker. It
also needs a debounce state machine (`Watcher.pending`) so a half-written
file is not loaded.

**Proposed change.** This changes behavior, so it needs a decision:

- **Recommended: check on window focus.** When egui reports the window gained
  focus, hash the sources once (in `spawn_blocking`) and reload if changed.
  Editing in another window and clicking back reloads immediately, with no
  timer and no idle wakeups. By then the save has long settled, so `Watcher`'s
  debounce and the interval task go away. `lua_sources` stays. About -40 lines.
  The fallback view's text ("save it: the window reloads") becomes "...when
  you come back to the window".
- **If reload-while-unfocused matters** (editor and window side by side): keep
  the timer but run the scan in `spawn_blocking` and poll every 2 s. About
  +3 lines. A `notify` (inotify) watcher would be exact but adds a dependency.

## 7. A running turn rebuilds the whole tree at 10 Hz

**Problem.** `spinner()` in `nodes/window.lua` calls `bone.gui.invalidate(100)`
while a turn runs, so the entire window is rebuilt in Lua ten times a second
to animate one glyph. The rebuild is cheap (~0.1 ms, chat items stay cached)
and the repaint is unavoidable for any animation; the waste is only the
rebuild.

**Proposed change.** Use a `canvas` node that draws the glyph (`fps = 10`):
the engine calls only its `draw`, nothing else is rebuilt, and no Rust
changes. It needs an explicit size (one monospace cell, which the tree can get
from #15). If that is judged more awkward than the saving, leave it; this is
the lowest-value item here.

## 8. The welcome field

Already optimized (value-noise lattice precomputed, mesh reused between ticks,
`field-noise` ~1.1 ms per tick at 20 fps) and only drawn on the welcome
screen. **No change.**

---

## 9. Dead code and stale comments

Concrete deletions, none behavioral:

- `unicode-width` in `crates/bone-gui/Cargo.toml`: no use in `src/` or
  `tests/`.
- `Host::geometry` and `Host::chat_at` overrides in `lua.rs`:
  `lua.geometry` is never written (always `Default`) and `chat_at` always
  returns `None`. Give both default bodies on the `Host` trait in `bone-ui`,
  delete the GUI's fields/overrides and the `Gui::chat_at` method; the shared
  `chat_at` op then answers `nil` without GUI code (check the op's dispatch
  in `bone-ui` first).
- Orphaned doc comments left by the refactor: in `Gui` (`lib.rs`), "Width (in
  columns) the current chat's rows were last wrapped at. Move the chat on
  screen…" above `visuals_rev`, and "A free-text answer being typed to the
  open question." above `lua`; in `LuaSide`, "Every message shown, and the
  last Lua errors in full." above `jobs`; and `watch_config`'s doc, which
  begins with the description of a function that no longer exists ("Reload
  the Lua config… call every frame").
- Clippy: `thread_local` initializers in `field.rs:246` and `profile.rs:12`
  can be `const`; `nodeview.rs:2298` has a collapsible `if`; `field.rs:369`
  test uses `.iter().any(|d| *d == 0)` for `.contains(&0)`.
- `panel_node` in `nodes/window.lua` passes `height = 800` to panel content.
  Pass the real window height (from #15's `ctx`) or omit it.
- `docs/plans/gui-vs-tui.md:65` still cites `screen.rs`; the deleted-files
  list in `gui-native-view.md:139` is accurate but should say "removed", not
  "to remove", once this lands.

## 10. The chat cache is used as a "changed?" flag

**Problem.** The GUI's `ChatBuffer<()>` stores empty `Vec<()>` line caches
(free in memory) so the shared `stale()`/`store()` machinery can say which
items need rebuilding. The cost is conceptual: `store(item, key, Vec::new(),
..)` reads as though lines were cached, and the freshness rule lives inside a
type parameterized over line data.

**Proposed change.** None beyond #2. With `stale_key` as the one definition of
freshness and a doc line on `ChatBuffer<()>` saying that is what the GUI uses
it for, the oddity is documented and small. Splitting freshness into its own
type would touch the TUI's `render_chat` for no behavior gain.

## 11. Windows and panels are still sized in character cells

**Problem.** The node view has no cells, but `bone.ui.popup`/`win`/`panel` are
shared with the TUI and sized in columns and rows. So the GUI keeps a bridge:
`cell_size`, `window_units`, `popups_for_lua`, `popup_mouse` (~250 lines in
`nodeview.rs`), the `gui_popups` and `gui_popup_mouse` ops, `lines_window` in
`nodes/popups.lua`, the `cell` field of `window_info`, and `resize` reported
in columns/rows. Built-in windows (settings, help, health) already draw as
nodes and use none of the row/column mouse path; only popups written for the
TUI (`lines = …`) do.

**Proposed change.**

- **Always:** move the bridge out of `nodeview.rs` into `popups.rs`, so the
  node view is not also the cell emulator. No line change.
- **Decision for the owner:** must TUI-authored `lines` popups keep working in
  the GUI? If yes, keep everything above, in `popups.rs`. If no (only `node`
  windows are supported in the GUI, `lines` raises "use `node =` in the GUI"),
  delete `popups_for_lua`'s line layout, `popup_mouse`, `gui_popup_mouse`,
  `lines_window` and `window_info.cell`: roughly -250 Rust and -45 Lua, and
  `cell_size` shrinks to the one place `resize` needs it.

## 12. `nodeview.rs` is 2,773 lines and does eight jobs

**Problem.** One file holds view state, the Lua ops, popup placement, list
virtualization, focus, wheel handling, painting, and the shape helpers.

**Proposed change.** A mechanical split with no behavior change, done after
#2/#3/#5 so those diffs stay reviewable:

| New file | From `nodeview.rs` | ~Lines |
|---|---|---|
| `ops.rs` | `dispatch`, `window_spec` | 270 |
| `popups.rs` | #11's bridge | 250 |
| `list.rs` | `realize*`, `chat_items`, `chat_item`, `make_item`, `ListState` | 330 |
| `focus.rs` | `settle_focus`, `move_focus`, `wheel`, `call_*` | 200 |
| `paint.rs` | `paint`, `fade*`, `scrollbar`, `*_input_ui`, `image_ui` | 900 |
| `shapes.rs` | `draw_border`, `gradient_shape`, `clip_between`, `cursor_icon`, `to_corners` | 260 |
| `nodeview.rs` | `ViewState`, `view_ui`, `Fonts`/`Metrics` | ~560 |

## 13. `LuaSide` mixes host state with the view's

**Problem.** `LuaSide` has ~50 fields. About a third (`view`, `view_error`,
`fallback`, `view_state`, `canvases`, `window_nodes`, `cell`, `screen_*`,
`fonts`, `extra_fonts`, `faces`) belong to the node view, so view code reaches
through `gui.lua.view_state.x` and `gui.lua.cell`, and `reload_lua`'s
`mem::take(&mut self.lua)` swaps view state with the interpreter.

**Proposed change.** Move those into a `ViewState` (or a `ViewCfg` beside it),
leaving `LuaSide` as the interpreter plus what `Host` borrows. Do it with #12,
since the new modules are where the paths change. Net ~0; fewer
`gui.lua.view_state.…` chains and a smaller `reload_lua` blast radius. When
the library is reloaded, `ViewState` keeps its scroll offsets and list
heights, which `mem::take` currently throws away, so a Lua reload also stops
snapping the chat.

## 14. The dispatch chain clones its arguments

**Problem.** `dispatch` in `lua.rs` tries `nodeview`, `images`, `chats`,
`prompt`, `system`, `window` in turn with `a.clone()`, because each returns
`Option<Result<…>>` and consumes its arguments before knowing whether it owns
the op. An op in `window` clones the argument list five times.

**Proposed change.** Give each module the signature `bone_ui::lua::dispatch`
already has: take `MultiValue`, return `Result<mlua::Result<MultiValue>,
MultiValue>` with `Err(a)` handing the arguments back unconsumed. The chain
becomes `.or_else(|a| images::dispatch(.., a))` with no clones. About -10
lines (the `.clone()`s and `Some(..)`/`.unwrap_or_else` plumbing).

## 15. `window_info()` goes through JSON on every build

**Problem.** `bone.gui.window_info()` builds a `serde_json::Value` and
converts it to a Lua table; Lua code (`panel_node`, popups) calls it during
every build for `cell`.

**Proposed change.** Add `cell = { w, h }` to the `ctx` that `build_tree`
already creates (`{ width, height }`), and pass `ctx` where `window_info()`
was used. Keep `window_info()` for plugins, built directly with `create_table`
instead of via JSON. About -15 lines.

---

## Status

Done and checked (`cargo clippy --workspace --all-targets` clean,
`cargo test --workspace` green, perf run above): #1 to #7, #9, #12 to #15.

- **#13** `ViewState` is now `Gui.view`, not part of `LuaSide`. What belongs to
  one Lua state (the view and fallback functions, window node functions,
  registered fonts, canvases, the tree and its handler table) is `ViewState.bound`,
  swapped out whole by `unbind()` when a reload starts and put back if the
  new configuration is refused. Scroll offsets and measured list heights stay
  through a reload; the cell size, screen size and font state live in
  `ViewState` too. Checked: a reload leaves the chat scroll offset where it
  was, and a refused one keeps drawing.
- **Streaming (new, not in the first list).** Remaking a growing answer on
  every delta cost 8.4 ms at 40 KB (2.3 ms at 10 KB), linear in length, so
  quadratic over an answer. A running chat item is now remade at most every
  50 ms (`STREAM_EVERY` in `nodeview/list.rs`), the view waking when the time
  is up so the last delta always shows; an item that is the same node at the
  same width is not measured again; text outside the clip is not shaped.
  Frame time at a 16 ms delta cadence: 40 KB 7.9 to 3.2 ms (median 1.0), 10 KB
  2.3 to 1.2 ms (median 0.55). The rest is remaking the item (markdown, then
  `node::parse`), which is per node, not per field lookup: reading each node
  table once instead of ~90 times per node saved only ~17% of that.
- **`node.rs` parsing** now reads a table once (`Props`), and names enums once
  (`names!`) instead of at each use: -80 lines.
- **Canvas** ops are egui shapes made when the draw function's result is read
  (`Op::Shape`), not six kinds painted by hand.

#11 turned out to have users: `/setup` (which opens by itself on a first run),
`/catalog`, `bone.ui.select` (the update prompt) and `bone.ui.pager` (a shell's
output) were `lines` windows in the GUI. They are nodes now
(`desktop/setup.lua`, `catalog.lua`, `pick.lua`), through
`bone.ui.windows.<name>`, over the same state and keys the TUI uses; the
cell-mouse emulation (`popup_mouse`, `gui_popup_mouse`, mouse mapping in
`lines_window`) is gone. A plugin's `lines` window still draws, as plain
monospace text without the pointer.

Not done: #8 (no change intended).
Source lines in `crates/bone-gui/src` are 10,005, against 10,115 when this
round began; the first round of this plan had grown them, and the remaining
large cut is #11.

## Order of work

Each step is independently shippable and verified with `cargo test -p bone-gui`
plus the perf run above.

1. **Hygiene (#9, clippy, #14).** Small, safe, no behavior change.
2. **Memory (#1).** The only unbounded growth.
3. **Chat path (#2 + #3 together, then #4, #5, #15).** The scaling fix; check
   `realize` at 24 and 1000 turns.
4. **Structure (#12 + #13).** The mechanical split, once the logic above has
   settled.
5. **Decisions needed before work:** #6 (reload on focus vs. timer) and #11
   (keep TUI-authored `lines` popups in the GUI or not). #7 is optional.

Estimated total: -150 to -200 lines for steps 1 to 4, and up to about -500
if #11 and #6 are taken in their deleting direction.
