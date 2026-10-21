# UiState, Host, and the shared Lua dispatcher

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

```rust
pub struct App {           // bone-tui; bone-gui has its own
    pub ui: bone_ui::UiState,
    pub tui: TuiState,     // implements bone_ui::Host
}
```

- `bone_ui::UiState` is everything about the client that is not drawing.
- `bone_ui::Host` is what only the frontend can answer or do.
- `bone_ui::dispatch(ui, host, lua, op, args) -> Option<Result<..>>`
  handles the shared ops and returns `None` for ops it does not know; the
  frontend's own dispatcher handles those (`tui::dispatch`).
- `with_api` stays the lending point. It splits the borrow
  (`let App { ui, tui } = self`) and gives both halves to the scope closure.

The split borrow is the reason `Host` is implemented by `TuiState`, not
`App`: a `Host` method that took `&mut App` could not be called while
`UiState` is borrowed.

### Re-entering Lua

Shared ops run Lua callbacks (`press` runs a keymap, `cmd` runs a command,
`opt_set` runs the option's `on_change`). Today that is `app.call_callback`, which calls
`with_api` again. In the split, the shared dispatcher gets a
`&mut dyn FnMut(&mut UiState, &mut dyn Host, u64, Args)` re-entry hook
(or `Host::call_callback`), so a nested call borrows the same two halves
instead of the whole `App`. The first migrated group that fires callbacks
(options, because of `on_change`) proves this works before the
rest move.

## What goes where

### `UiState` (shared)

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

### `TuiState` (frontend only)

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

It stays small on purpose: anything that is state goes in `UiState`, and the
trait only covers questions about the screen and the OS.

## The ops

Grouped in the order they migrate. Each group lands as one commit, moving its
state into `UiState` and its ops into `bone_ui::dispatch`, with its
`docs/lua.md` entries classified and its conformance tests added.

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
- **Render cache line type.** `UiState.chats` is `Vec<ChatBuffer<L>>`. Making
  `UiState` generic over `L` is the simplest; a boxed `dyn Any` line cache
  is the alternative if the generic spreads too far.
- **`api.lua` split.** Ops move first; the `bone.*` wrappers in
  `runtime/tui/api.lua` move to `runtime/ui/api.lua` per group, in the same
  commit as the ops they wrap.
