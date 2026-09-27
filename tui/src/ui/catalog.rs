//! `/catalog` fullscreen popup, driven by the shared catalog screen.

use std::io;

use bone_protocol::{CatalogAction, CatalogApplyResult, CatalogSnapshot};
pub use bone_render::screens::catalog::*;
use crossterm::event::{self, Event, KeyEventKind};

use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::theme::Theme;

/// Run the catalog popup against a daemon-host apply callback.
pub fn run<F>(theme: &Theme, snapshot: CatalogSnapshot, apply: F) -> io::Result<Outcome>
where
    F: FnMut(String, Vec<CatalogAction>) -> Result<CatalogApplyResult, String>,
{
    fullscreen::run(|term| run_loop(term, theme, snapshot, apply))
}

fn run_loop<F>(
    term: &mut FullscreenTerminal,
    theme: &Theme,
    snapshot: CatalogSnapshot,
    mut apply: F,
) -> io::Result<Outcome>
where
    F: FnMut(String, Vec<CatalogAction>) -> Result<CatalogApplyResult, String>,
{
    let mut screen = CatalogScreen::new(snapshot, theme);
    term.draw(|frame| screen.draw(frame, theme))?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match screen.handle_key(crate::ui::screen_key(key)) {
                    CatalogKeyAction::Close => return Ok(screen.outcome().clone()),
                    CatalogKeyAction::Apply { revision, actions } => {
                        let result = apply(revision, actions);
                        screen.applied(result, theme);
                    }
                    CatalogKeyAction::None => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        term.draw(|frame| screen.draw(frame, theme))?;
    }
}
