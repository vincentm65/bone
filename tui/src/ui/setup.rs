//! First-launch onboarding wizard, driven by the shared setup screen.

use std::io;

use bone_protocol::SetupSnapshot;
pub use bone_render::screens::setup::*;
use crossterm::event::{self, Event, KeyEventKind};

use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::theme::Theme;

/// Run the onboarding wizard and return its mutation plan, or `None` on cancel.
pub fn run(theme: &Theme, fresh: bool, snapshot: SetupSnapshot) -> io::Result<Option<Plan>> {
    fullscreen::run(|term| run_loop(term, fresh, snapshot, theme))
}

fn run_loop(
    term: &mut FullscreenTerminal,
    fresh: bool,
    snapshot: SetupSnapshot,
    theme: &Theme,
) -> io::Result<Option<Plan>> {
    let mut screen = SetupScreen::new(fresh, snapshot, theme);
    term.draw(|frame| screen.draw(frame, theme))?;
    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match screen.handle_key(crate::ui::screen_key(key)) {
                SetupAction::Cancel => return Ok(None),
                SetupAction::Submit(plan) => return Ok(Some(plan)),
                SetupAction::None => {}
            }
        }
        term.draw(|frame| screen.draw(frame, theme))?;
    }
}
