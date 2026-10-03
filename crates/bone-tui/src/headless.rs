//! Drive the TUI without a terminal: type and press keys, pump server events, read the
//! screen as text. Used by end-to-end tests; it runs exactly the same code as
//! [`crate::run`] except that frames go to an in-memory buffer.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bone_client::{Client, EventStream};
use bone_proto::Connection;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use crate::RunOptions;
use crate::app::{App, AppEvent};
use crate::keys;

pub struct Headless {
    app: App,
    rx: mpsc::UnboundedReceiver<AppEvent>,
    events: EventStream,
    terminal: Terminal<TestBackend>,
}

impl Headless {
    /// Connect, load the config (like `run`), and size the virtual screen.
    pub async fn start(
        conn: Connection,
        opts: RunOptions,
        width: u16,
        height: u16,
    ) -> io::Result<Self> {
        let (client, events) = Client::new(conn);
        client
            .initialize("bone-headless")
            .await
            .map_err(|e| io::Error::other(format!("cannot connect to the bone server: {e}")))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let mut app = App::new(Arc::new(client), tx, opts.cwd, opts.config_dir);
        app.load_user_config();
        if let Some(id) = opts.resume {
            app.resume(id);
        }
        let terminal = Terminal::new(TestBackend::new(width, height))?;
        Ok(Headless {
            app,
            rx,
            events,
            terminal,
        })
    }

    /// Type text, one key per character (newlines press enter).
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == '\n' {
                keys::parse("enter").unwrap()
            } else {
                keys::Key::char(c)
            };
            self.app.handle_key(key);
        }
    }

    /// Press keys by name, space-separated: `"enter"`, `"ctrl+c ctrl+c"`.
    pub fn press(&mut self, names: &str) -> Result<(), String> {
        for name in names.split_whitespace() {
            self.app.handle_key(keys::parse(name)?);
        }
        Ok(())
    }

    pub fn paste(&mut self, text: &str) {
        self.app.paste(text);
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.terminal.backend_mut().resize(width, height);
        self.app.dirty = true;
    }

    /// Handle replies, server events and key timeouts until nothing has
    /// happened for `quiet`.
    pub async fn settle(&mut self, quiet: Duration) {
        loop {
            tokio::select! {
                Some(ev) = self.rx.recv() => self.app.apply(ev),
                Some(ev) = self.events.recv() => self.app.handle_server(ev),
                _ = tokio::time::sleep(quiet) => return,
            }
        }
    }

    /// Keep handling events until the screen satisfies `pred`, or fail with
    /// the last screen after `timeout`.
    pub async fn wait_for(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen();
            if pred(&screen) {
                return Ok(screen);
            }
            if Instant::now() >= deadline {
                return Err(screen);
            }
            self.settle(Duration::from_millis(20)).await;
        }
    }

    /// Draw a frame and return it as text, one line per row, trailing spaces
    /// trimmed.
    pub fn screen(&mut self) -> String {
        let app = &mut self.app;
        self.terminal
            .draw(|f| crate::render::draw(f, app))
            .expect("in-memory draw");
        let buf = self.terminal.backend().buffer();
        let area = buf.area;
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Which keymaps apply: `"main"` or `"popup"`.
    pub fn context(&self) -> &'static str {
        self.app.context().name()
    }

    /// Set once `/quit` ran or the server went away; `Some(reason)` for the latter.
    pub fn quit(&self) -> Option<Option<String>> {
        self.app.quit.clone()
    }
}
