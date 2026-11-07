//! Drive the GUI without a window: the same [`Gui`] and frames, with text
//! laid out by fixed advances (a char is `size` × 0.6 wide). Used by the
//! multi-client and conformance tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bone_proto::Connection;

use crate::backend::text::{
    Family, FixedShaper, FontSet, FontsApplied, Job, Shaped, Shaper, fixed_layout_by,
};
use crate::backend::{
    Backend, Button, DrawList, Event, FrameInput, Key, Modifiers, Rect, Shape, pos, vec2,
};
use crate::{Gui, GuiOptions};

/// Fixed advances, except in a Lua-registered family, whose chars take
/// their widths from its file.
#[derive(Default)]
struct NoWindowShaper {
    /// Per family: each char's advance in ems.
    families: HashMap<String, HashMap<char, f32>>,
}

impl Shaper for NoWindowShaper {
    fn shape(&self, job: Job) -> Arc<Shaped> {
        let (size, rows) = fixed_layout_by(&job, |style, c| match &style.family {
            Family::Named(name) => self.families.get(name)?.get(&c).copied(),
            _ => None,
        });
        Arc::new(Shaped {
            text: job.text.clone(),
            size,
            rows,
            handle: Box::new(job),
        })
    }

    fn row_height(&self, family: &Family, size: f32) -> f32 {
        FixedShaper.row_height(family, size)
    }

    fn mono_cell(&self, size: f32) -> (f32, f32) {
        FixedShaper.mono_cell(size)
    }
}

/// Each char's advance in ems in font file `bytes`.
fn advances(bytes: &[u8]) -> Option<HashMap<char, f32>> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let em = f32::from(face.units_per_em());
    let mut out = HashMap::new();
    for c in (' '..='~').chain('\u{a0}'..='\u{17f}') {
        if let Some(adv) = face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)) {
            out.insert(c, f32::from(adv) / em);
        }
    }
    Some(out)
}

#[derive(Default)]
struct NoWindow {
    shaper: NoWindowShaper,
}

impl Backend for NoWindow {
    fn shaper(&self) -> &dyn Shaper {
        &self.shaper
    }

    /// Fonts are read as the window would, so a file that cannot be read
    /// is reported; only registered families change the layout.
    fn set_fonts(&mut self, set: &FontSet) -> FontsApplied {
        let read = |p: &str| std::fs::read(p).ok().and_then(|b| advances(&b));
        let mut errors = Vec::new();
        let mut faces = [false; 3];
        if let Some(p) = &set.mono
            && read(p).is_none()
        {
            errors.push(p.clone());
        }
        for (i, p) in set.sans.iter().enumerate() {
            let Some(p) = p else { continue };
            match read(p) {
                Some(_) => faces[i] = true,
                None => errors.push(p.clone()),
            }
        }
        self.shaper.families.clear();
        for (name, p) in &set.families {
            match read(p) {
                Some(adv) => {
                    self.shaper.families.insert(name.clone(), adv);
                }
                None => errors.push(p.clone()),
            }
        }
        FontsApplied {
            faces: (faces[1], faces[2]),
            errors,
        }
    }
}

pub struct Headless {
    pub gui: Gui,
    /// What was last copied to the clipboard.
    pub copied: Option<String>,
    /// When the last frame asked for the next one (`Duration::MAX`: only
    /// on input).
    pub repaint: Duration,
    /// The text last frame painted, each piece with the rectangle it
    /// covers, in painting order.
    pub painted: Vec<(String, Rect)>,
    /// The filled rectangles it painted (boxes, bubbles, bars).
    pub boxes: Vec<Rect>,
    /// Those with the color each was filled with (red, green, blue, alpha).
    pub fills: Vec<(Rect, [u8; 4])>,
    /// The last frame's shapes.
    pub draw: DrawList,
    size: (f32, f32),
    /// Input for the next frame, as the window would send it.
    events: Vec<Event>,
    mods: Modifiers,
    scale: f32,
    /// Whether the window has the focus. Gaining it checks the Lua
    /// config for a saved edit.
    focused: bool,
    /// Seconds spent in [`Gui::frame`] last [`Headless::frame`].
    pub ui_secs: f64,
    backend: NoWindow,
}

impl Headless {
    pub async fn start(
        conn: Connection,
        opts: GuiOptions,
        width: f32,
        height: f32,
    ) -> Result<Self, String> {
        Ok(Headless {
            gui: Gui::connect(conn, opts).await?,
            copied: None,
            repaint: Duration::MAX,
            painted: Vec::new(),
            boxes: Vec::new(),
            fills: Vec::new(),
            draw: DrawList::default(),
            size: (width, height),
            events: Vec::new(),
            mods: Modifiers::NONE,
            scale: 1.0,
            focused: true,
            ui_secs: 0.0,
            backend: NoWindow::default(),
        })
    }

    /// Lay out one frame, as the window would draw it.
    pub fn frame(&mut self) {
        let mut input = FrameInput::new(Rect::from_min_size(pos(0.0, 0.0), vec2(self.size.0, self.size.1)));
        input.events = std::mem::take(&mut self.events);
        input.scale = self.scale;
        input.focused = self.focused;
        input.mods = self.mods;
        let started = std::time::Instant::now();
        let out = self.gui.frame(input, &mut self.backend);
        self.ui_secs = started.elapsed().as_secs_f64();
        self.painted.clear();
        self.boxes.clear();
        self.fills.clear();
        for item in &out.draw.items {
            match &item.shape {
                Shape::Text { at, text, .. } => {
                    self.painted.push((text.text.clone(), text.rect_at(*at)));
                }
                Shape::Rect { rect, fill, .. } if fill.3 > 0 => {
                    self.boxes.push(*rect);
                    self.fills.push((*rect, [fill.0, fill.1, fill.2, fill.3]));
                }
                _ => {}
            }
        }
        self.draw = out.draw;
        self.repaint = out.repaint_after.unwrap_or(Duration::MAX);
        if let Some(t) = out.copy {
            self.copied = Some(t);
        }
    }

    /// Handle replies and server events until nothing has happened for
    /// `quiet`, then draw a frame.
    pub async fn settle(&mut self, quiet: Duration) {
        while let Ok(true) = tokio::time::timeout(quiet, self.gui.next()).await {}
        self.frame();
    }

    /// Keep handling events until `pred` holds for the GUI, or fail with the
    /// transcript after `timeout`.
    pub async fn wait_for(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&Gui) -> bool,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.frame();
            if pred(&self.gui) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(self.gui.transcript());
            }
            self.settle(Duration::from_millis(20)).await;
        }
    }

    /// The window's size in logical points, from the next frame.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.size = (width, height);
    }

    /// The display's scale (2.0 for a HiDPI screen), from the next frame.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    /// Whether the window has the focus, from the next frame. Gaining it
    /// reloads Lua when the config on disk changed.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Press `key` with `modifiers` in the next frame.
    pub fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.events.push(Event::Key {
                key,
                pressed,
                repeat: false,
                mods: modifiers,
            });
        }
    }

    /// Press and release the left button over character cell `(x, y)`
    /// from the window's top left (the cell measured last frame) in the
    /// next frame.
    pub fn click(&mut self, x: u16, y: u16) {
        let (col, row) = self.gui.view.cell.expect("a frame was drawn");
        self.click_at((f32::from(x) + 0.5) * col, (f32::from(y) + 0.5) * row);
    }

    /// Move the pointer to logical pixel `(x, y)`.
    pub fn hover_at(&mut self, x: f32, y: f32) {
        self.events.push(Event::PointerMoved(pos(x, y)));
    }

    /// Press (or release) the left button at logical pixel `(x, y)`.
    pub fn button_at(&mut self, x: f32, y: f32, pressed: bool) {
        let p = pos(x, y);
        self.events.push(Event::PointerMoved(p));
        self.events.push(Event::PointerButton {
            pos: p,
            button: Button::Primary,
            pressed,
            mods: Modifiers::NONE,
        });
    }

    /// Press and release the left button at logical pixel `(x, y)`.
    pub fn click_at(&mut self, x: f32, y: f32) {
        self.button_at(x, y, true);
        self.button_at(x, y, false);
    }

    /// Drop file `path` on the window in the next frame.
    pub fn drop_file(&mut self, path: PathBuf) {
        self.events.push(Event::DroppedFile(path));
    }

    /// Paste `text` in the next frame.
    pub fn paste(&mut self, text: &str) {
        self.events.push(Event::Paste(text.to_owned()));
    }

    /// Type `text` in the next frame.
    pub fn text(&mut self, text: &str) {
        self.events.push(Event::Text(text.to_owned()));
    }

    /// Scroll the wheel by `dx`/`dy` points in the next frame.
    pub fn wheel(&mut self, dx: f32, dy: f32) {
        self.events.push(Event::Wheel {
            delta: vec2(dx, dy),
            unit: crate::backend::input::WheelUnit::Point,
            mods: Modifiers::NONE,
        });
    }

    /// Send any input event in the next frame (another button, the wheel
    /// with keys held).
    pub fn event(&mut self, ev: Event) {
        self.events.push(ev);
    }

    /// Type `text` into the prompt and send it, as Enter does.
    pub fn send(&mut self, text: &str) {
        self.gui.draft = text.to_owned();
        self.gui.submit();
    }
}
