# Host and the shared Lua dispatcher

Design for Phase 3 of `gui-roadmap.md`: how the Lua API gets one
implementation used by both frontends.

## Today

`bone-tui::App` holds everything. Lua reaches it through one string-op
dispatcher, `dispatch(app, lua, op, args)` in `lua.rs` (about 125 ops), which
`App::with_api` lends `&mut App` to inside `lua.scope`. `runtime/tui/api.lua`
wraps the ops as `bone.*` functions. Callbacks Lua stores (keymaps, commands,
autocmds, popups) are ids into the `bone.callbacks` registry table, and
running one goes back through `with_api`, so ops can re-enter Lua and Lua can
re-enter ops.

## Target

Each frontend keeps its own application struct (`bone-tui::App`,
`bone-gui::Gui`) and implements one trait:

```rust
pub trait Host {
    // Shared state, as `bone-ui` types the frontend stores where it likes.
    fn option_store(&mut self) -> OptionStore<'_>;   // options, dynamic, carried, rev
    fn owners(&mut self) -> Owners<'_>;              // plugin ownership, callback ids
    // ...one accessor per migrated group...

    // Services only the frontend can provide.
    fn call_callback_multi(&mut self, id: u64, context: &str, args: ...) -> Option<MultiValue>;
    fn changed(&mut self);                           // something to draw
    // ...geometry, clipboard, redraw timers as their groups move...
}
```

- `bone_ui::lua::dispatch(host, lua, op, args)` handles the shared ops and
  hands back the arguments of an op it does not know, for the frontend's own
  dispatcher.
- `with_api` stays the lending point: it lends the whole frontend struct, as
  today.
- Accessors return bundles of borrows (`OptionStore { options, dynamic, .. }`)
  over the frontend's own fields, so moving an op group does not move or
  rename TUI fields.

### Re-entering Lua

Shared ops run Lua callbacks (`press` runs a keymap, `cmd` runs a command,
`opt_set` runs the option's `on_change`). They do it through
`Host::call_callback_multi`, which is the frontend's existing
`call_callback_multi`: it runs as the callback's owner and re-enters
`with_api` on the whole struct. Nothing is split, so there is no borrow to
juggle; an accessor's borrows end before a callback runs.

This replaces an earlier draft of this note that split `App` into a shared
`UiState` and a `TuiState`. The split made re-entry need a two-half borrow
and would have renamed every TUI field; the accessor trait does neither.
The tables below still say which state is shared: it is the state the
accessors expose.

## What goes where

### Shared (behind `Host` accessors)

| Group | Fields |
| --- | --- |
| chats | `chats`, `current`, `rendering`, `lua_item_seq`, `render_deps`, `render_expires`, `chat_expiry` |
| prompt | `prompt`, `composer`, `last_prompt_state` |
| keys | `keymaps`, `focused_context`, `pending_keys`, `pending_action`, `pending_context`, `pending_since`, `raw_interceptors`, `in_actions` |
| options | `options`, `dynamic_options`, `opts_rev`, `carried_options` |
| commands and events | `user_commands`, `autocmds` |
| looks | `theme` (bone-ui's), `colors_name`, `spinner`, `views_rev`, `ui_broken`, `ui_expiry` |
| popups and panels | `popups`, `panels`, `panel_focus` (state only) |
| plugins and ownership | `owner`, `callback_owner`, `owned`, `keymap_owner`, `plugins`, `shutdown_hooks`, `next_callback` |
| core | `client`, `asks`, `settings`, `processes`, `processes_version`, `processes_session`, `terms`, `term_sizes`, `term_resize_pending` |
| Lua | `lua`, `config_dir`, `load_errors`, `load_failures`, `lua_errors` |
| messages | `message`, `log`, `quit`, `dirty`, `redraw` |

`jobs` stays per frontend until the PTY question below is settled.

### Frontend only

`windows`, `chat_views`, `seps`, `placed`, `leaves`, `screen`, `selection`,
`clipboard`, `clipboard_executable`, `title`, `quit_armed`, `tx`, `cwd`,
the terminal, and the ratatui line cache type for `ChatBuffer<L>`.

`windows` and `chat_views` are scroll state in rows. They stay per frontend:
the GUI scrolls in pixels and maps to items, not rows.

### `Host`

```rust
pub trait Host {
    /// The frontend's side name and capabilities, for `bone.api_info`.
    fn side(&self) -> bone_lua::Side;
    /// Where layout leaves, panels and popups were last drawn, in character
    /// units (`bone.ui.geometry`, `chat_at`).
    fn geometry(&self) -> Geometry;
    /// The chat item and line at a point, for mouse hits and `chat_at`.
    fn chat_hit(&self, buf: BufferId, row: usize) -> Option<(usize, usize)>;
    fn scroll_chat(&mut self, buf: BufferId, to: Scroll);
    fn clipboard_copy(&mut self, text: String);
    fn clipboard_paste(&mut self) -> Option<String>;
    /// Ask for a frame; `Some(at)` for a timed one.
    fn request_redraw(&mut self, at: Option<Instant>);
    /// Frontend checks for `/health`.
    fn health(&self) -> Vec<HealthItem>;
}
```

These services sit on `Host` beside the state accessors, added as the groups
that need them move.

## The ops

Grouped in the order they migrate. Each group lands as one commit, adding
its `Host` accessors and moving its ops into `bone_ui::lua::dispatch`, with
its `docs/lua.md` entries classified and its conformance tests added.

| # | Group | Ops | Needs from `Host` |
| --- | --- | --- | --- |
| 1 | text, markdown | (pure functions, already bound without `App`) | — |
| 2 | options | `opt_get`, `opt_set`, `opt_define`, `opt_del`, `opt_names`, `opt_info`, `opt_apply`, `opt_list` | redraw |
| 3 | highlights | `hl_set`, `hl_get`, `hl_reset`, `hl_names`, `colorscheme`, `colors_name` | redraw |
| 4 | events and commands | `on`, `off`, `cmd`, `command_create`, `command_del`, `command_list`, `command_find`, `command_complete`, `history_add` | — |
| 5 | keymaps | `keymap_*`, `press`, `action` | redraw |
| 6 | prompt | `prompt_*`, `image_attach`, `image_remove` | redraw |
| 7 | chat data | `chat_items`, `chat_add`, `chat_update`, `chat_remove`, `chat_turns`, `chat_session`, `chat_sessions`, `chat_refresh_in`, `chat_redraw`, `open_session`, `session` | redraw |
| 8 | core | `request`, `wait`, `notify`, `settings_get`, `process_*` | — |
| 9 | popups and panels | `popup_*`, `panel_*`, `ui_refresh`, `ui_refresh_in` | geometry, redraw |
| 10 | plugins and config | `plugin_*`, `exec_lua`, `source_file`, `project_trust`, `project_info`, `library_*`, `runtime_overrides`, `runtime_reset`, `log_tail`, `show_message`, `health` | side, health |
| 11 | screen | `geometry`, `chat_at`, `chat_view`, `chat_scroll`, `chat_scroll_to`, `clipboard_copy`, `clipboard_paste` | geometry, hits, scroll, clipboard |
| 12 | jobs | `job_*` | see below |

Group 11 is "shared with units": the same ops, answered by the host in
character units. Nothing in today's op list is TUI-only once group 11 is
expressed that way; TUI-only behavior is in options (synchronized updates)
and in what `runtime/tui/api.lua` does with the results.

## Open questions

- **PTY jobs.** `bone.job` with `pty` runs a pseudo-terminal in the TUI
  process. `bone-ui` has no PTY dependency by rule. Either the job runner
  becomes a `Host` capability (`gui.terminal` arrives later anyway), or
  `jobs.rs` moves to a small shared crate that both frontends depend on.
  Decide when group 12 starts.
- **Render cache line type.** Chats are `ChatBuffer<L>` with the frontend's
  line type; the chat accessor is generic over `L` (an associated type on
  `Host`).
- **`api.lua` split.** Ops move first; the `bone.*` wrappers in
  `runtime/tui/api.lua` move to `runtime/ui/api.lua` per group, in the same
  commit as the ops they wrap.
