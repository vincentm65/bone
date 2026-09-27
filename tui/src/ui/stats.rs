//! Fullscreen usage-stats dashboard.

use std::io;

use bone_render::screens::stats::{StatsAction, StatsScreen};
use crossterm::event::{self, Event, KeyEventKind};

use crate::session_db::{DateRange, UsageStatsSnapshot};
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::theme::Theme;

pub fn run<F>(theme: &Theme, mut load: F) -> io::Result<()>
where
    F: FnMut(&Option<DateRange>) -> io::Result<UsageStatsSnapshot>,
{
    fullscreen::run(|term| run_loop(term, theme, &mut load))
}

fn run_loop<F>(term: &mut FullscreenTerminal, theme: &Theme, load: &mut F) -> io::Result<()>
where
    F: FnMut(&Option<DateRange>) -> io::Result<UsageStatsSnapshot>,
{
    let (mut screen, first) = StatsScreen::new(theme);
    if let StatsAction::Load(range) = first {
        screen.loaded(Ok(load(&range)?));
    }
    term.draw(|frame| screen.draw(frame, theme))?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match screen.handle_key(crate::ui::screen_key(key)) {
                    StatsAction::Close => break,
                    StatsAction::Load(range) => {
                        screen.loaded(load(&range).map_err(|error| error.to_string()));
                    }
                    StatsAction::None => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => continue,
        }
        term.draw(|frame| screen.draw(frame, theme))?;
    }
    Ok(())
}
