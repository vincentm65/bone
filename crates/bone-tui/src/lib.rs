//! Fullscreen bone TUI.
//!
//! One view: the session above, a prompt below. There are no modes: you
//! always type, `/` starts a command, and popups (from Lua, or the session
//! picker) take the keyboard while open. Keymaps, commands, colors and views
//! come from Lua. It talks to the bone core only through a
//! [`bone_client::Client`], so it runs the same against an in-process server
//! or a socket.

mod app;
mod chat;
mod clipboard;
mod commands;
mod composer;
mod data;
mod editor;
mod headless;
mod jobs;
mod keymap;
mod keys;
mod layout;
mod lua;
mod markdown;
mod options;
mod panel;
mod plugins;
mod render;
mod selection;
mod shellhl;
mod term;
mod terminal;
mod text;
mod theme;
mod ui;

#[cfg(test)]
mod tests;

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bone_client::Client;
use bone_proto::Connection;
use crossterm::event::{Event, EventStream, KeyEventKind, MouseButton, MouseEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::app::App;
#[doc(hidden)]
pub use crate::clipboard::run_clipboard_helper;
pub use crate::headless::Headless;

/// At most this often while content streams in.
const FRAME: Duration = Duration::from_millis(16);
/// How often Lua files are checked for changes.
const LUA_RELOAD_POLL: Duration = Duration::from_millis(250);

pub struct RunOptions {
    /// Executable implementing the internal clipboard helper; None uses this executable.
    pub clipboard_executable: Option<std::path::PathBuf>,
    /// Working directory for new sessions.
    pub cwd: String,
    /// Where `tui.lua`, `lua/` modules and runtime overrides live.
    pub config_dir: Option<std::path::PathBuf>,
    /// Resume a session at startup: `Some(None)` for the newest.
    pub resume: Option<Option<String>>,
    /// The core runs in this process, from the same config dir, so edits
    /// to its Lua files reload it (`core/reload`). Off for `--connect`.
    pub reload_core: bool,
}

/// Run the TUI on `conn` until the user quits. Returns a message to print
/// after the terminal is restored, if the session ended abnormally.
pub async fn run(conn: Connection, opts: RunOptions) -> io::Result<Option<String>> {
    let (client, mut server_events) = Client::new(conn);
    client
        .initialize("bone-tui")
        .await
        .map_err(|e| io::Error::other(format!("cannot connect to the bone server: {e}")))?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Saved settings apply before the user's tui.lua, so fetch them first.
    let settings = client
        .request::<bone_proto::methods::SettingsGet>(bone_proto::methods::Empty {})
        .await
        .unwrap_or_default();
    let mut app = App::new(Arc::new(client), tx, opts.cwd, opts.config_dir);
    app.reload_core = opts.reload_core;
    app.clipboard_executable = opts.clipboard_executable;
    app.settings = settings;
    app.load_user_config();
    app.note_runtime_overrides();
    let mut watched_project = app.project_dir();
    // A real interval: a sleep made anew each pass would never fire while
    // output streams or a spinner draws.
    let mut lua_poll = tokio::time::interval(LUA_RELOAD_POLL);
    lua_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watching = true;
    let mut lua_pending = None;
    let mut lua_state = lua_snapshot(app.config_dir.as_deref(), watched_project.as_deref());
    if let Some(id) = opts.resume {
        app.resume(id);
    }

    let mut term = terminal::enter()?;
    let mut mouse = app.options.mouse;
    terminal::set_mouse(mouse)?;
    // Always typing, so always a bar cursor.
    crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::SetCursorStyle::SteadyBar
    )?;
    let mut input = EventStream::new();
    let mut last_draw = Instant::now() - FRAME;

    while app.quit.is_none() {
        let draw_at = if app.dirty {
            Some((last_draw + FRAME).max(Instant::now()))
        } else if app.animating() {
            Some(last_draw + Duration::from_millis(app.spinner.interval_ms.max(16)))
        } else {
            None
        };
        // Chat view timers and UI-only redraws share the frame rate limit.
        let expiry = app
            .chat_expiry
            .into_iter()
            .chain(app.ui_expiry)
            .min()
            .map(|e| e.max(last_draw + FRAME));
        let draw_at = match (draw_at, expiry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timeout_at = app.key_sequence_deadline();
        tokio::select! {
            biased;
            ev = rx.recv() => {
                if let Some(ev) = ev {
                    app.apply(ev);
                }
            }
            ev = input.next() => match ev {
                Some(Ok(ev)) => handle_terminal(&mut app, ev),
                Some(Err(e)) => return Err(e),
                None => break,
            },
            ev = server_events.recv() => match ev {
                Some(ev) => app.handle_server(ev),
                None => app.server_closed(),
            },
            _ = sleep_until(timeout_at.unwrap_or_else(Instant::now).into()), if timeout_at.is_some() => {
                app.expire_key_sequence();
            }
            _ = sleep_until(draw_at.unwrap_or_else(Instant::now).into()), if draw_at.is_some() => {
                term.draw_frame(|f| render::draw(f, &mut app))?;
                app.dirty = std::mem::take(&mut app.redraw);
                if let Some(title) = app.ui_title()
                    && app.title.as_ref() != Some(&title)
                {
                    crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(&title))?;
                    app.title = Some(title);
                }
                if let Some(seq) = app.clipboard.take().and_then(|t| selection::copy(&t)) {
                    crossterm::execute!(std::io::stdout(), crossterm::style::Print(seq))?;
                }
                if app.options.mouse != mouse {
                    mouse = app.options.mouse;
                    terminal::set_mouse(mouse)?;
                }
                last_draw = Instant::now();
            }
            _ = lua_poll.tick(), if app.config_dir.is_some() => {
                if !app.options.autoreload {
                    watching = false;
                    continue;
                }
                // Turned back on: compare from now, not from before.
                if !watching {
                    watching = true;
                    lua_pending = None;
                    lua_state = lua_snapshot(app.config_dir.as_deref(), watched_project.as_deref());
                    continue;
                }
                // Trusting or untrusting the project (or a reload) changes
                // what is watched: start comparing again, without a reload.
                let project = app.project_dir();
                if project != watched_project {
                    watched_project = project;
                    lua_pending = None;
                    lua_state = lua_snapshot(app.config_dir.as_deref(), watched_project.as_deref());
                    continue;
                }
                let next = lua_snapshot(app.config_dir.as_deref(), watched_project.as_deref());
                let Some(changed) = settle(&mut lua_state, &mut lua_pending, next) else {
                    continue;
                };
                if changed.core && app.reload_core {
                    app.reload_core_config();
                }
                if changed.tui {
                    app.reload_user_config();
                }
            }
        }
    }
    app.shutdown();
    drop(term);
    Ok(app.quit.flatten())
}

/// What each side's Lua files looked like, as hashes of paths and metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LuaSources {
    tui: u64,
    core: u64,
}

/// Which sides changed, once a change has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Changed {
    tui: bool,
    core: bool,
}

/// Take `next` only once it has stayed the same for a whole poll, so a file
/// caught halfway through being written (or a save touching several files)
/// is not loaded. `pending` is what the last poll saw that differed from
/// `state`. Returns which sides changed when `state` moves on.
fn settle(
    state: &mut LuaSources,
    pending: &mut Option<LuaSources>,
    next: LuaSources,
) -> Option<Changed> {
    if next == *state {
        *pending = None;
        return None;
    }
    if *pending != Some(next) {
        *pending = Some(next);
        return None;
    }
    let changed = Changed {
        tui: next.tui != state.tui,
        core: next.core != state.core,
    };
    *state = next;
    *pending = None;
    Some(changed)
}

/// Which side loads a file, by its path under the config dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watched {
    Tui,
    Core,
    Both,
}

/// `tui.lua`, `colors/` and `runtime/tui/` are the TUI's; `core.lua` and
/// `runtime/core/` the core's, and the same inside a plugin. Modules (`lua/`,
/// `runtime/lua/`) can be required by either.
fn side_of(rel: &Path) -> Watched {
    let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
    let parts = match parts.as_slice() {
        ["plugins", _, rest @ ..] => rest,
        ["runtime", rest @ ..] => rest,
        all => all,
    };
    match parts {
        ["tui.lua"] | ["tui", ..] | ["colors", ..] => Watched::Tui,
        ["core.lua"] | ["core", ..] => Watched::Core,
        _ => Watched::Both,
    }
}

/// A deliberately small, dependency-free watcher. Editors commonly save by
/// replacing files, so we hash paths and metadata instead of holding file
/// handles open. Reloading is handled on the TUI task, keeping Lua callbacks
/// single-threaded and preserving the active session. A trusted project's
/// `.bone/` (`project`) is the TUI's only.
fn lua_snapshot(dir: Option<&Path>, project: Option<&Path>) -> LuaSources {
    let Some(dir) = dir else {
        return LuaSources { tui: 0, core: 0 };
    };
    let mut files = Vec::new();
    // Only where Lua is loaded from: the config dir also holds sessions
    // and other data that would be walked on every poll.
    for name in ["tui.lua", "core.lua"] {
        let path = dir.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    for sub in ["lua", "runtime", "colors"] {
        collect_lua_files(&dir.join(sub), &mut files);
    }
    // Only the plugins that load (not `_name` or `.name`).
    for plugin in bone_lua::plugins(dir) {
        collect_lua_files(&plugin, &mut files);
    }
    files.sort();
    let mut tui = DefaultHasher::new();
    let mut core = DefaultHasher::new();
    for path in &files {
        let side = side_of(path.strip_prefix(dir).unwrap_or(path));
        if side != Watched::Core {
            hash_file(path, &mut tui);
        }
        if side != Watched::Tui {
            hash_file(path, &mut core);
        }
    }
    if let Some(project) = project {
        let mut files = Vec::new();
        collect_lua_files(project, &mut files);
        files.sort();
        for path in &files {
            hash_file(path, &mut tui);
        }
    }
    LuaSources {
        tui: tui.finish(),
        core: core.finish(),
    }
}

fn hash_file(path: &Path, h: &mut DefaultHasher) {
    path.hash(h);
    if let Ok(meta) = fs::metadata(path) {
        meta.len().hash(h);
        meta.modified().ok().hash(h);
    }
}

/// Every `*.lua` under `dir`, skipping hidden folders (a plugin's `.git`).
fn collect_lua_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !entry.file_name().to_string_lossy().starts_with('.') {
                collect_lua_files(&path, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "lua") {
            out.push(path);
        }
    }
}

fn button_name(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

fn handle_terminal(app: &mut App, ev: Event) {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => app.handle_key(k.into()),
        Event::Paste(text) => app.paste(&text),
        Event::Mouse(m) => {
            let at = (m.column, m.row);
            let code = match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if app.panel_wheel(at, m.kind == MouseEventKind::ScrollUp) =>
                {
                    return;
                }
                MouseEventKind::ScrollUp => crate::keys::WHEEL_UP,
                MouseEventKind::ScrollDown => crate::keys::WHEEL_DOWN,
                MouseEventKind::Down(b) => return app.mouse("down", button_name(b), at),
                MouseEventKind::Drag(b) => return app.mouse("drag", button_name(b), at),
                MouseEventKind::Up(b) => return app.mouse("up", button_name(b), at),
                _ => return,
            };
            app.handle_key(crate::keys::Key::new(code, m.modifiers));
        }
        Event::Resize(width, height) => app.resize(width, height),
        _ => {}
    }
}
