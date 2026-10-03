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
mod editor;
mod headless;
mod keymap;
mod keys;
mod layout;
mod lua;
mod markdown;
mod options;
mod render;
mod selection;
mod shellhl;
mod terminal;
mod text;
mod theme;
mod ui;

#[cfg(test)]
mod tests;

use std::io;
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
const ANIMATION: Duration = Duration::from_millis(100);

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
            Some(last_draw + ANIMATION)
        } else {
            None
        };
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
            _ = sleep_until(draw_at.unwrap_or_else(Instant::now).into()), if draw_at.is_some() => {
                term.draw(|f| render::draw(f, &mut app))?;
                app.dirty = false;
                if let Some(seq) = app.clipboard.take().and_then(|t| selection::copy(&t)) {
                    crossterm::execute!(std::io::stdout(), crossterm::style::Print(seq))?;
                }
                if app.options.mouse != mouse {
                    mouse = app.options.mouse;
                    terminal::set_mouse(mouse)?;
                }
                last_draw = Instant::now();
            }
        }
    }
    drop(term);
    Ok(app.quit.flatten())
}

fn handle_terminal(app: &mut App, ev: Event) {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => app.handle_key(k.into()),
        Event::Paste(text) => app.paste(&text),
        Event::Mouse(m) => {
            let at = (m.column, m.row);
            let code = match m.kind {
                MouseEventKind::ScrollUp => crate::keys::WHEEL_UP,
                MouseEventKind::ScrollDown => crate::keys::WHEEL_DOWN,
                MouseEventKind::Down(MouseButton::Left) => return app.mouse_down(at),
                MouseEventKind::Drag(MouseButton::Left) => return app.mouse_drag(at),
                MouseEventKind::Up(MouseButton::Left) => return app.mouse_up(at),
                _ => return,
            };
            app.handle_key(crate::keys::Key::new(code, m.modifiers));
        }
        Event::Resize(..) => app.dirty = true,
        _ => {}
    }
}
