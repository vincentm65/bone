//! Page tabs: the TUI's full-screen pages (`/stats`, `/setup`, `/catalog`, the
//! live process viewer, and agent transcripts) hosted in desktop tabs. Each
//! wraps a shared `bone_render` screen; the app carries out the effects a page
//! asks for over the connection of the chat tab that opened it.
use bone_protocol::{HostRequest, HostResponse, ProcessSnapshot, RuntimeCommand};
use bone_render::Message;
use bone_render::screens::{
    Key, TouchAction,
    catalog::{CatalogKeyAction, CatalogScreen},
    host,
    process::{ProcessAction, ProcessScreen},
    setup::{SetupAction, SetupScreen},
    stats::{StatsAction, StatsScreen},
    transcript::TranscriptScreen,
};
use bone_render::theme::Theme;

pub(crate) enum Screen {
    Stats(Box<StatsScreen>),
    /// `None` until the daemon's setup snapshot arrives.
    Setup(Option<SetupScreen>),
    /// `None` until the daemon's catalog snapshot arrives.
    Catalog(Option<CatalogScreen>),
    Process(ProcessScreen),
    Transcript(TranscriptScreen),
}

/// What the app must do for a page after a key or a reply.
pub(crate) enum Effect {
    None,
    Close,
    /// Close the page and post this line to its chat tab.
    CloseWith(String),
    /// Send a host request; its reply comes back through [`Page::response`].
    Request(HostRequest),
    Command(RuntimeCommand),
}

pub(crate) struct Page {
    pub screen: Screen,
    pub title: String,
    /// The chat tab whose connection serves this page.
    pub chat: u64,
    /// Request id of the host request in flight.
    pub pending: Option<u64>,
    /// A failure to show instead of the screen (e.g. the snapshot failed).
    pub error: Option<String>,
}

impl Page {
    fn new(screen: Screen, title: &str, chat: u64) -> Self {
        Self {
            screen,
            title: title.to_owned(),
            chat,
            pending: None,
            error: None,
        }
    }

    pub fn stats(chat: u64, theme: &Theme) -> (Self, Effect) {
        let (screen, action) = StatsScreen::new(theme);
        let effect = stats_effect(action);
        (
            Self::new(Screen::Stats(Box::new(screen)), "stats", chat),
            effect,
        )
    }

    pub fn setup(chat: u64) -> (Self, Effect) {
        (
            Self::new(Screen::Setup(None), "setup", chat),
            Effect::Request(HostRequest::Setup),
        )
    }

    pub fn catalog(chat: u64) -> (Self, Effect) {
        (
            Self::new(Screen::Catalog(None), "catalog", chat),
            Effect::Request(HostRequest::Catalog { refresh: true }),
        )
    }

    pub fn process(chat: u64, process: ProcessSnapshot) -> (Self, Effect) {
        let title = format!("$ {}", one_line(&process.command, 24));
        (
            Self::new(Screen::Process(ProcessScreen::new(process)), &title, chat),
            Effect::Command(RuntimeCommand::GetProcesses),
        )
    }

    pub fn transcript(chat: u64, title: &str, messages: Vec<Message>) -> Self {
        Self::new(
            Screen::Transcript(TranscriptScreen::new(messages, false)),
            title,
            chat,
        )
    }

    pub fn handle_key(&mut self, key: Key) -> Effect {
        if self.error.is_some() {
            return Effect::Close;
        }
        match &mut self.screen {
            Screen::Stats(screen) => stats_effect(screen.handle_key(key)),
            Screen::Setup(Some(screen)) => match screen.handle_key(key) {
                SetupAction::None => Effect::None,
                SetupAction::Cancel => Effect::CloseWith("Setup cancelled.".into()),
                SetupAction::Submit(plan) => Effect::Request(host::setup_apply_request(plan)),
            },
            Screen::Catalog(Some(screen)) => match screen.handle_key(key) {
                CatalogKeyAction::None => Effect::None,
                CatalogKeyAction::Close => Effect::CloseWith(screen.outcome().message.clone()),
                CatalogKeyAction::Apply { revision, actions } => {
                    Effect::Request(host::catalog_apply_request(revision, actions))
                }
            },
            Screen::Process(screen) => match screen.handle_key(key) {
                ProcessAction::None => Effect::None,
                ProcessAction::Close => Effect::Close,
                ProcessAction::Cancel(id) => Effect::Command(RuntimeCommand::CancelProcess { id }),
            },
            Screen::Transcript(screen) => {
                if screen.handle_key(key) {
                    Effect::Close
                } else {
                    Effect::None
                }
            }
            // Still loading: only Esc leaves.
            Screen::Setup(None) | Screen::Catalog(None) => {
                if key.code == bone_render::screens::KeyCode::Esc {
                    Effect::Close
                } else {
                    Effect::None
                }
            }
        }
    }

    /// Map a touch on an existing page row/footer to the same key path as the
    /// keyboard. Full-screen geometry is supplied by the native cell painter.
    pub fn touch_key(&self, row: u16, col: u16, cols: u16, rows: u16) -> Option<TouchAction> {
        match &self.screen {
            Screen::Stats(screen) => screen.touch_key(row, col, cols, rows),
            Screen::Setup(Some(screen)) => screen.touch_key(row, col, cols, rows),
            Screen::Catalog(Some(screen)) => screen.touch_key(row, col, cols, rows),
            Screen::Process(screen) => screen
                .touch_key(row, col, cols, rows)
                .map(|key| TouchAction::Keys(vec![key])),
            Screen::Transcript(screen) => screen
                .touch_key(row, col, cols, rows)
                .map(|key| TouchAction::Keys(vec![key])),
            _ => None,
        }
    }

    /// Whether this page exposes the setup provider API-key editor.
    pub fn setup_api_key_available(&self) -> bool {
        matches!(&self.screen, Screen::Setup(Some(screen)) if screen.api_key_editor_available())
    }

    /// Terminal rows occupied by the setup provider API-key field.
    pub fn setup_api_key_rows(&self, rows: u16) -> Option<(u16, u16)> {
        match &self.screen {
            Screen::Setup(Some(screen)) => screen.api_key_rows(rows),
            _ => None,
        }
    }

    /// Borrow the setup provider API key for the native frontend's invisible
    /// egui editor.
    pub fn setup_api_key_mut(&mut self) -> Option<&mut String> {
        match &mut self.screen {
            Screen::Setup(Some(screen)) => screen.api_key_mut(),
            _ => None,
        }
    }

    /// Turn a vertical touch swipe into repeated page keys. This preserves the
    /// existing bounded scroll state instead of maintaining a second viewport.
    pub fn swipe(&mut self, delta_y: f32, row_height: f32) -> Effect {
        if !matches!(
            self.screen,
            Screen::Stats(_) | Screen::Process(_) | Screen::Transcript(_)
        ) {
            return Effect::None;
        }
        let (code, count) = swipe_plan(delta_y, row_height);
        let mut effect = Effect::None;
        for _ in 0..count {
            effect = self.handle_key(Key::plain(code));
            if !matches!(effect, Effect::None) {
                break;
            }
        }
        effect
    }

    /// Mouse-wheel scrolling, in notches (positive scrolls down).
    pub fn scroll(&mut self, notches: i64) {
        let lines = notches * bone_render::screens::transcript::MOUSE_WHEEL_LINES as i64;
        match &mut self.screen {
            Screen::Transcript(screen) => screen.scroll_by(lines),
            Screen::Process(screen) => {
                let code = if notches > 0 {
                    bone_render::screens::KeyCode::Down
                } else {
                    bone_render::screens::KeyCode::Up
                };
                for _ in 0..lines.unsigned_abs() {
                    screen.handle_key(Key::plain(code));
                }
            }
            _ => {}
        }
    }

    /// Deliver the reply to this page's pending host request.
    pub fn response(&mut self, response: HostResponse, theme: &Theme) -> Effect {
        self.pending = None;
        match &mut self.screen {
            Screen::Stats(screen) => {
                screen.loaded(host::stats(Ok(response)));
                Effect::None
            }
            Screen::Setup(slot @ None) => match host::setup_snapshot(Ok(response)) {
                Ok(snapshot) => {
                    *slot = Some(SetupScreen::new(false, snapshot, theme));
                    Effect::None
                }
                Err(message) => Effect::CloseWith(format!("Setup wizard failed: {message}")),
            },
            Screen::Setup(Some(_)) => match host::setup_applied(Ok(response)) {
                Ok(result) => {
                    let suffix = if result.restart_required {
                        " Restart bone to load the new provider, tools, and commands."
                    } else {
                        ""
                    };
                    Effect::CloseWith(format!("{}{suffix}", result.message))
                }
                Err(message) => Effect::CloseWith(format!("Setup wizard failed: {message}")),
            },
            Screen::Catalog(slot @ None) => match host::catalog_snapshot(Ok(response)) {
                Ok(snapshot) => {
                    *slot = Some(CatalogScreen::new(snapshot, theme));
                    Effect::None
                }
                Err(message) => Effect::CloseWith(format!("Catalog failed: {message}")),
            },
            Screen::Catalog(Some(screen)) => {
                screen.applied(host::catalog_applied(Ok(response)), theme);
                Effect::None
            }
            Screen::Process(_) | Screen::Transcript(_) => Effect::None,
        }
    }

    /// Apply a fresh process list; returns false when a process page's process
    /// is gone and the tab should close.
    pub fn update_processes(&mut self, processes: &[ProcessSnapshot]) -> bool {
        match &mut self.screen {
            Screen::Process(screen) => screen.update(processes),
            _ => true,
        }
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame, theme: &Theme) {
        let loading = |frame: &mut ratatui::Frame, text: &str| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(text.to_owned())
                    .style(ratatui::style::Style::default().fg(theme.status_text)),
                frame.area(),
            );
        };
        if let Some(error) = &self.error {
            loading(frame, &format!("{error}\n\nPress any key to close."));
            return;
        }
        match &mut self.screen {
            Screen::Stats(screen) => screen.draw(frame, theme),
            Screen::Setup(Some(screen)) => screen.draw(frame, theme),
            Screen::Catalog(Some(screen)) => screen.draw(frame, theme),
            Screen::Process(screen) => screen.draw(frame, theme),
            Screen::Transcript(screen) => screen.draw(frame, theme),
            Screen::Setup(None) => loading(frame, "Loading setup…"),
            Screen::Catalog(None) => loading(frame, "Loading catalog…"),
        }
    }
}

fn swipe_plan(delta_y: f32, row_height: f32) -> (bone_render::screens::KeyCode, usize) {
    let code = if delta_y < 0.0 {
        bone_render::screens::KeyCode::Down
    } else {
        bone_render::screens::KeyCode::Up
    };
    let count = ((delta_y.abs() / row_height.max(1.0)).round() as usize).clamp(1, 32);
    (code, count)
}

fn stats_effect(action: StatsAction) -> Effect {
    match action {
        StatsAction::None => Effect::None,
        StatsAction::Close => Effect::Close,
        StatsAction::Load(range) => Effect::Request(HostRequest::Stats { range }),
    }
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{CatalogItem, CatalogSnapshot, ProviderChoice, SetupSnapshot};
    use bone_render::screens::{KeyCode, TouchAction};

    fn setup_page() -> Page {
        let mut screen = SetupScreen::new(
            false,
            SetupSnapshot {
                config_revision: 1,
                providers: vec![ProviderChoice {
                    id: "demo".into(),
                    label: "Demo".into(),
                    api_key_configured: false,
                    api_key_required: true,
                }],
                active_provider: "demo".into(),
                init_exists: false,
                needs_onboarding: false,
                catalog: CatalogSnapshot {
                    revision: "catalog-1".into(),
                    items: Vec::new(),
                },
            },
            &Theme::default(),
        );
        assert!(matches!(
            screen.handle_key(Key::plain(KeyCode::Right)),
            SetupAction::None
        ));
        Page {
            screen: Screen::Setup(Some(screen)),
            title: "setup".into(),
            chat: 1,
            pending: None,
            error: None,
        }
    }

    #[test]
    fn touch_key_dispatches_setup_catalog_stats_and_api_key_actions() {
        let setup = setup_page();
        assert!(matches!(
            setup.touch_key(8, 4, 80, 20),
            Some(TouchAction::Keys(_))
        ));
        assert_eq!(setup.touch_key(16, 4, 80, 20), Some(TouchAction::ApiKey));

        let catalog = Page {
            screen: Screen::Catalog(Some(CatalogScreen::new(
                CatalogSnapshot {
                    revision: "catalog-1".into(),
                    items: vec![CatalogItem {
                        name: "demo".into(),
                        description: "Demo".into(),
                        ..CatalogItem::default()
                    }],
                },
                &Theme::default(),
            ))),
            title: "catalog".into(),
            chat: 1,
            pending: None,
            error: None,
        };
        assert!(matches!(
            catalog.touch_key(7, 3, 80, 20),
            Some(TouchAction::Keys(_))
        ));

        let (stats, _) = Page::stats(1, &Theme::default());
        assert_eq!(
            stats.touch_key(2, 2, 80, 20),
            Some(TouchAction::Keys(vec![Key::plain(KeyCode::Char('d'))]))
        );
    }

    #[test]
    fn page_swipes_preserve_the_existing_vertical_key_direction_and_count() {
        assert_eq!(
            swipe_plan(-4.0, 2.0),
            (KeyCode::Down, 2),
            "an upward finger swipe reveals later rows"
        );
        assert_eq!(swipe_plan(4.0, 2.0), (KeyCode::Up, 2));
        assert_eq!(swipe_plan(100.0, 1.0), (KeyCode::Up, 32));
    }
}
