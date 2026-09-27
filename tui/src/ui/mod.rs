//! Terminal UI (feature `ui`): ratatui app, rendering, input, panes, and commands.

pub mod app;
pub mod autocomplete;
pub mod catalog;
pub mod color;
pub mod commands;
pub mod fullscreen;
pub mod host;
pub mod input;
pub mod jobs_pane;
pub mod pane_page;
pub mod picker;
pub mod process_view;
pub mod processes_pane;
pub mod prompt;
pub mod queue_pane;
pub mod render;
pub(crate) mod selectable_pane;
pub mod setup;
pub mod stats;
pub mod theme;
pub(crate) mod timing;
pub mod tool_display;
pub mod transcript_view;

/// Convert a terminal key press into the key type shared screens match on.
pub fn screen_key(key: crossterm::event::KeyEvent) -> bone_render::screens::Key {
    use bone_render::screens::KeyCode as Screen;
    use crossterm::event::{KeyCode, KeyModifiers};
    let code = match key.code {
        KeyCode::Char(c) => Screen::Char(c),
        KeyCode::Enter => Screen::Enter,
        KeyCode::Esc => Screen::Esc,
        KeyCode::Tab => Screen::Tab,
        KeyCode::BackTab => Screen::BackTab,
        KeyCode::Backspace => Screen::Backspace,
        KeyCode::Delete => Screen::Delete,
        KeyCode::Up => Screen::Up,
        KeyCode::Down => Screen::Down,
        KeyCode::Left => Screen::Left,
        KeyCode::Right => Screen::Right,
        KeyCode::PageUp => Screen::PageUp,
        KeyCode::PageDown => Screen::PageDown,
        KeyCode::Home => Screen::Home,
        KeyCode::End => Screen::End,
        _ => Screen::Other,
    };
    bone_render::screens::Key {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}
