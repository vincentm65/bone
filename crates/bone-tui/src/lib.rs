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
mod commands;
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
pub use crate::headless::Headless;

/// At most this often while content streams in.
const FRAME: Duration = Duration::from_millis(16);
/// Redraw rate for spinners and elapsed time.
const LUA_RELOAD_POLL: Duration = Duration::from_millis(250);

pub struct RunOptions {
    /// Working directory for new sessions.
    pub cwd: String,
    /// Where `tui.lua`, `lua/` modules and runtime overrides live.
    pub config_dir: Option<std::path::PathBuf>,
    /// Resume a session at startup: `Some(None)` for the newest.
    pub resume: Option<Option<String>>,
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
    let mut app = App::new(Arc::new(client), tx, opts.cwd, opts.config_dir);
    app.load_user_config();
    let mut lua_state = lua_snapshot(app.config_dir.as_deref());
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
        // A chat view asked to be drawn again (bone.chat.refresh_in).
        let expiry = app.chat_expiry.map(|e| e.max(last_draw + FRAME));
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
                term.draw(|f| render::draw(f, &mut app))?;
                app.dirty = false;
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
            _ = tokio::time::sleep(LUA_RELOAD_POLL), if app.config_dir.is_some() => {
                let next = lua_snapshot(app.config_dir.as_deref());
                if next != lua_state {
                    lua_state = next;
                    app.reload_user_config();
                }
            }
        }
    }
    app.shutdown();
    drop(term);
    Ok(app.quit.flatten())
}

/// A deliberately small, dependency-free watcher. Editors commonly save by
/// replacing files, so we hash paths and metadata instead of holding file
/// handles open. Reloading is handled on the TUI task, keeping Lua callbacks
/// single-threaded and preserving the active session.
fn lua_snapshot(dir: Option<&Path>) -> u64 {
    let Some(dir) = dir else { return 0 };
    let mut files = Vec::new();
    // Only where TUI Lua is loaded from: the config dir also holds sessions
    // and other data that would be walked on every poll.
    let tui = dir.join("tui.lua");
    if tui.is_file() {
        files.push(tui);
    }
    for sub in ["lua", "runtime", "colors", "plugins"] {
        collect_lua_files(&dir.join(sub), &mut files);
    }
    files.sort();
    let mut h = DefaultHasher::new();
    for path in files {
        path.hash(&mut h);
        if let Ok(meta) = fs::metadata(&path) {
            meta.len().hash(&mut h);
            meta.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

fn collect_lua_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lua_files(&path, out);
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
