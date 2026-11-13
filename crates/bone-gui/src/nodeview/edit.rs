//! A text field: the prompt and the view's text inputs. Its text is edited
//! with the keys and text left for it this frame, and the pointer places
//! the cursor and selects; it paints itself: the text (or the placeholder),
//! the selection, the blinking cursor, and the input method's text being
//! composed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::input::{Event, Key, Modifiers};
use crate::backend::interact::Sense;
use crate::backend::text::{Job, RunStyle, Shaped};
use crate::backend::{Cursor, Rect, Shape, pos, vec2};
use crate::node::{Corners, Rgba};

use super::Fx;

/// What a field keeps between frames besides its text.
#[derive(Clone, Debug, Default)]
pub(crate) struct EditState {
    /// How far the text is scrolled (down a multi-line field, along a
    /// one-line one).
    pub scroll: f32,
    /// The pointer went down here and is selecting.
    pub selecting: bool,
    /// When the caret's blink cycle started: none while the field or the
    /// window is without the keyboard, and afresh at each edit and at each
    /// return of the keyboard. See [`CARET_PERIOD`].
    pub caret_at: Option<Instant>,
}

/// The caret's cycle, in milliseconds: lit for the first [`CARET_ON`] of
/// each `CARET_PERIOD`, then away for the rest. A field that shows a caret
/// asks to be drawn again at each edge of the cycle.
const CARET_PERIOD: u64 = 1000;
const CARET_ON: u64 = 500;

/// Text with a cursor and a selection, in chars.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Field {
    pub text: String,
    /// The other end of the selection (equal to `cursor`: none).
    pub anchor: usize,
    pub cursor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

impl Field {
    pub fn new(text: String, anchor: usize, cursor: usize) -> Self {
        let n = text.chars().count();
        Field {
            text,
            anchor: anchor.min(n),
            cursor: cursor.min(n),
        }
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.anchor != self.cursor)
            .then(|| (self.anchor.min(self.cursor), self.anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.text.chars().skip(a).take(b - a).collect())
    }

    fn byte(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Replace chars `a..b` with `with`; the cursor goes after it.
    pub fn replace(&mut self, a: usize, b: usize, with: &str) {
        let (a, b) = (a.min(b), a.max(b));
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.text.replace_range(ba..bb, with);
        self.cursor = a + with.chars().count();
        self.anchor = self.cursor;
    }

    /// Typing replaces the selection.
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        let s: String = s.chars().filter(|c| *c != '\r').collect();
        self.replace(a, b, &s);
    }

    /// Delete the selection, if there is one.
    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.replace(a, b, "");
                true
            }
            None => false,
        }
    }

    fn word_back(&self, from: usize) -> usize {
        let chars = self.chars();
        let mut i = from;
        while i > 0 && class(chars[i - 1]) == Class::Space {
            i -= 1;
        }
        if i > 0 {
            let c = class(chars[i - 1]);
            while i > 0 && class(chars[i - 1]) == c {
                i -= 1;
            }
        }
        i
    }

    fn word_forward(&self, from: usize) -> usize {
        let chars = self.chars();
        let mut i = from;
        if i < chars.len() {
            let start = class(chars[i]);
            while i < chars.len() && class(chars[i]) == start && start != Class::Space {
                i += 1;
            }
        }
        while i < chars.len() && class(chars[i]) == Class::Space {
            i += 1;
        }
        i
    }

    fn line_start(&self, from: usize) -> usize {
        let chars = self.chars();
        let mut i = from.min(chars.len());
        while i > 0 && chars[i - 1] != '\n' {
            i -= 1;
        }
        i
    }

    fn line_end(&self, from: usize) -> usize {
        let chars = self.chars();
        let mut i = from.min(chars.len());
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        i
    }

    /// The word around char `at`.
    pub fn word_at(&self, at: usize) -> (usize, usize) {
        let chars = self.chars();
        if chars.is_empty() {
            return (0, 0);
        }
        let at = at.min(chars.len() - 1);
        let c = class(chars[at]);
        let (mut a, mut b) = (at, at + 1);
        while a > 0 && class(chars[a - 1]) == c {
            a -= 1;
        }
        while b < chars.len() && class(chars[b]) == c {
            b += 1;
        }
        (a, b)
    }

    /// Move the cursor to `to`, the selection following with `extend`.
    fn go(&mut self, to: usize, extend: bool) {
        self.cursor = to.min(self.len());
        if !extend {
            self.anchor = self.cursor;
        }
    }
}

/// How a field should look and behave.
pub(crate) struct Look<'a> {
    pub style: RunStyle,
    pub placeholder: &'a str,
    pub placeholder_color: Rgba,
    pub selection: Rgba,
    pub cursor: Rgba,
    pub password: bool,
    pub multiline: bool,
    pub read_only: bool,
    /// Enter is a newline (else it is left for submitting).
    pub enter_newline: bool,
    /// Styles over char ranges: `(from, to, style)`, later ones winning.
    pub marks: Vec<(usize, usize, RunStyle)>,
    /// Text drawn after the cursor when it ends its line.
    pub ghost: Option<(String, Rgba)>,
}

/// What happened to a field this frame.
#[derive(Default)]
pub(crate) struct Outcome {
    pub changed: bool,
    /// Enter (or ctrl+enter in a many-line field) was pressed.
    pub submitted: bool,
    /// Esc or tab: it gives up the keyboard.
    pub escaped: bool,
    pub tabbed: Option<bool>,
    /// The pointer went down on it.
    pub pressed: bool,
    pub clicked: bool,
    /// The cursor or selection moved.
    pub moved: bool,
}

impl crate::Gui {
    /// Edit and paint field `key` in `area`, cut to `clip`; with `focused`
    /// it takes this frame's keys and text.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::nodeview) fn field_ui(
        &mut self,
        fx: &mut Fx,
        key: &str,
        area: Rect,
        clip: Rect,
        f: &mut Field,
        focused: bool,
        look: &Look,
    ) -> Outcome {
        let mut out = Outcome::default();
        let before = (f.anchor, f.cursor);
        if focused {
            for ev in std::mem::take(&mut fx.typed) {
                self.field_event(fx, f, ev, look, &mut out);
            }
        }
        let visible = area.intersect(clip);
        // A read-only field leaves the pointer to the node around it (a
        // tiled chat's draft focuses that chat when pressed).
        let resp = if look.read_only {
            Default::default()
        } else {
            (self.view.pointer).sense(&format!("field:{key}"), visible, Sense::Drag)
        };
        if resp.hovered {
            fx.out.cursor = Cursor::Text;
        }
        let shaped = self.field_shape(fx, f, look, area.width());
        let state = self.view.edits.entry(key.to_owned()).or_default();
        let origin = |state: &EditState| {
            if look.multiline {
                pos(area.min.x, area.min.y - state.scroll)
            } else {
                pos(area.min.x - state.scroll, area.min.y)
            }
        };
        if let (true, Some(p)) = (resp.press_started, resp.pointer) {
            let at = shaped.index_at(pos(p.x, p.y) - origin(state).to_vec2());
            let shift = self.view.pointer.mods.shift;
            f.go(at, shift);
            state.selecting = true;
            out.pressed = true;
        }
        if resp.double_clicked
            && let Some(p) = resp.pointer
        {
            let at = shaped.index_at(pos(p.x, p.y) - origin(state).to_vec2());
            let (a, b) = f.word_at(at);
            (f.anchor, f.cursor) = (a, b);
            state.selecting = false;
        } else if state.selecting
            && let Some(p) = self.view.pointer.pos
        {
            let at = shaped.index_at(pos(p.x, p.y) - origin(state).to_vec2());
            f.go(at, true);
            if !self.view.pointer.is_down(crate::backend::Button::Primary) {
                state.selecting = false;
            }
        }
        out.clicked = resp.clicked;
        out.moved = (f.anchor, f.cursor) != before;
        // Keep the cursor in view.
        let (c, line) = shaped.cursor_pos(f.cursor.min(shaped.char_count()));
        if look.multiline {
            let h = area.height();
            if c.y - state.scroll < 0.0 {
                state.scroll = c.y;
            } else if c.y + line - state.scroll > h {
                state.scroll = c.y + line - h;
            }
            state.scroll = state.scroll.clamp(0.0, (shaped.size.y - h).max(0.0));
        } else {
            let w = area.width() - 2.0;
            if c.x - state.scroll < 0.0 {
                state.scroll = c.x;
            } else if c.x - state.scroll > w {
                state.scroll = c.x - w;
            }
            state.scroll = state.scroll.clamp(0.0, (shaped.size.x - w).max(0.0));
        }
        let at = origin(state);
        let inside = visible;
        // The selection, under the text.
        if let Some((a, b)) = f.selection() {
            for r in shaped.range_rects(a, b) {
                fx.paint(
                    Shape::Rect {
                        rect: r.translate(at.to_vec2()),
                        corners: Corners::default(),
                        fill: look.selection,
                    },
                    inside,
                );
            }
        }
        if f.text.is_empty() && self.view.preedit.is_empty() || !focused && f.text.is_empty() {
            let mut st = look.style.clone();
            st.color = look.placeholder_color;
            let hint = fx.shaper.shape(Job::simple(
                look.placeholder.to_owned(),
                st,
                Some(area.width().max(1.0)),
            ));
            fx.paint(
                Shape::Text {
                    at: area.min,
                    text: hint,
                    color: None,
                },
                inside,
            );
        } else {
            fx.paint(
                Shape::Text {
                    at,
                    text: shaped.clone(),
                    color: None,
                },
                inside,
            );
        }
        if focused {
            let shown = f.cursor + self.view.preedit.chars().count();
            let (c, line) = shaped.cursor_pos(shown.min(shaped.char_count()));
            // A thin caret, and at a line's start it stands just before the
            // first glyph (the placeholder's too), not over it.
            let lead = if c.x < 0.5 { -1.5 } else { 0.0 };
            let caret = Rect::from_min_size(at + c.to_vec2() + vec2(lead, 0.0), vec2(1.5, line));
            // Ghost text when the cursor ends its line.
            if let Some((ghost, color)) = &look.ghost
                && !ghost.is_empty()
                && f.selection().is_none()
                && f.text.chars().nth(f.cursor).is_none_or(|c| c == '\n')
            {
                let mut st = look.style.clone();
                st.color = *color;
                let line_text = ghost.lines().next().unwrap_or_default().to_owned();
                let g = fx.shaper.shape(Job::simple(line_text, st, None));
                fx.paint(
                    Shape::Text {
                        at: caret.min,
                        text: g,
                        color: None,
                    },
                    inside,
                );
            }
            // The caret blinks. Its cycle starts afresh with every edit,
            // move, press and composed character, so it is lit while typing.
            // A window without the keyboard shows no caret at all and asks
            // for no frames: the frame the focus change itself draws is what
            // wipes the half-blink the timer left standing, and the cycle
            // starts afresh on the next frame that has the keyboard, so the
            // caret is there the moment focus comes back.
            let typing = out.changed || out.moved || out.pressed || !self.view.preedit.is_empty();
            let blind = !fx.input.focused;
            if typing || blind {
                state.caret_at = None;
            }
            let lit = if blind {
                false
            } else {
                let now = Instant::now();
                let since = now - *state.caret_at.get_or_insert(now);
                let phase = since.as_millis() as u64 % CARET_PERIOD;
                let edge = if phase < CARET_ON { CARET_ON } else { CARET_PERIOD };
                fx.out.repaint_after(Duration::from_millis(edge - phase));
                phase < CARET_ON
            };
            if lit {
                // At a line's start the caret stands just left of the
                // field's area, which would cut it away whole.
                let room = Rect::from_min_max(inside.min - vec2(1.5, 0.0), inside.max);
                fx.paint(
                    Shape::Rect {
                        rect: caret,
                        corners: Corners::default(),
                        fill: look.cursor,
                    },
                    room.intersect(clip),
                );
            }
            fx.out.ime = Some(caret);
        } else {
            state.caret_at = None;
        }
        out
    }

    /// The field's text laid out as it is drawn: the password hidden, the
    /// marks over it, and the input method's text at the cursor.
    fn field_shape(&self, fx: &Fx, f: &Field, look: &Look, width: f32) -> Arc<Shaped> {
        let text: String = if look.password {
            "•".repeat(f.len())
        } else {
            f.text.clone()
        };
        let preedit = &self.view.preedit;
        let cursor = f.cursor.min(text.chars().count());
        let mut job = Job {
            text: String::new(),
            runs: Vec::new(),
            wrap: look.multiline.then_some(width.max(1.0)),
            ellipsis: false,
            align: crate::node::TextAlign::Left,
            box_width: None,
        };
        // The style of each char: a mark's, or the field's.
        let chars: Vec<char> = text.chars().collect();
        let mut style_of: Vec<usize> = vec![usize::MAX; chars.len()];
        for (k, (a, b, _)) in look.marks.iter().enumerate() {
            let (a, b) = ((*a).min(chars.len()), (*b).min(chars.len()));
            if a < b {
                style_of[a..b].fill(k);
            }
        }
        let style = |k: usize| {
            if k == usize::MAX {
                look.style.clone()
            } else {
                look.marks[k].2.clone()
            }
        };
        let mut i = 0;
        while i <= chars.len() {
            if i == cursor && !preedit.is_empty() {
                let mut st = look.style.clone();
                st.underline = true;
                job.append(preedit, st);
            }
            if i == chars.len() {
                break;
            }
            let k = style_of[i];
            let mut j = i;
            while j < chars.len() && style_of[j] == k && !(j > i && j == cursor && !preedit.is_empty())
            {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            job.append(&run, style(k));
            i = j;
        }
        if job.runs.is_empty() {
            job.append("", look.style.clone());
        }
        fx.shaper.shape(job)
    }

    fn field_event(&mut self, fx: &mut Fx, f: &mut Field, ev: Event, look: &Look, out: &mut Outcome) {
        let edit = !look.read_only;
        match ev {
            Event::Text(t) if edit => {
                if !look.multiline && t.contains('\n') {
                    f.insert(&t.replace('\n', " "));
                } else {
                    f.insert(&t);
                }
                self.view.preedit.clear();
                out.changed = true;
            }
            Event::Paste(t) if edit => {
                let t = if look.multiline {
                    t
                } else {
                    t.replace('\n', " ")
                };
                f.insert(&t);
                out.changed = true;
            }
            Event::Preedit(t) => self.view.preedit = t,
            Event::Key {
                key,
                pressed: true,
                mods,
                ..
            } => self.field_key(fx, f, key, mods, look, out),
            _ => {}
        }
    }

    fn field_key(
        &mut self,
        fx: &mut Fx,
        f: &mut Field,
        key: Key,
        m: Modifiers,
        look: &Look,
        out: &mut Outcome,
    ) {
        let edit = !look.read_only;
        let word = m.ctrl || m.alt;
        let shift = m.shift;
        match key {
            Key::ArrowLeft => {
                let to = match (f.selection(), shift, word) {
                    (Some((a, _)), false, false) => a,
                    (_, _, true) => f.word_back(f.cursor),
                    _ => f.cursor.saturating_sub(1),
                };
                f.go(to, shift);
            }
            Key::ArrowRight => {
                let to = match (f.selection(), shift, word) {
                    (Some((_, b)), false, false) => b,
                    (_, _, true) => f.word_forward(f.cursor),
                    _ => f.cursor + 1,
                };
                f.go(to, shift);
            }
            Key::ArrowUp | Key::ArrowDown if look.multiline => {
                // The same column a line up or down.
                let start = f.line_start(f.cursor);
                let col = f.cursor - start;
                let to = if key == Key::ArrowUp {
                    if start == 0 {
                        0
                    } else {
                        let prev = f.line_start(start - 1);
                        (prev + col).min(start - 1)
                    }
                } else {
                    let end = f.line_end(f.cursor);
                    if end >= f.len() {
                        f.len()
                    } else {
                        let next = end + 1;
                        (next + col).min(f.line_end(next))
                    }
                };
                f.go(to, shift);
            }
            Key::Home => {
                let to = if m.ctrl { 0 } else { f.line_start(f.cursor) };
                f.go(to, shift);
            }
            Key::End => {
                let to = if m.ctrl { f.len() } else { f.line_end(f.cursor) };
                f.go(to, shift);
            }
            Key::Backspace if edit => {
                if !f.delete_selection() && f.cursor > 0 {
                    let from = if word {
                        f.word_back(f.cursor)
                    } else {
                        f.cursor - 1
                    };
                    f.replace(from, f.cursor, "");
                }
                out.changed = true;
            }
            Key::Delete if edit => {
                if !f.delete_selection() && f.cursor < f.len() {
                    let to = if word {
                        f.word_forward(f.cursor)
                    } else {
                        f.cursor + 1
                    };
                    f.replace(f.cursor, to, "");
                }
                out.changed = true;
            }
            Key::Enter => {
                let newline = look.multiline
                    && if look.enter_newline {
                        !m.command()
                    } else {
                        shift
                    };
                if newline && edit {
                    f.insert("\n");
                    out.changed = true;
                } else if !look.multiline || m.command() {
                    out.submitted = true;
                }
            }
            Key::Escape => out.escaped = true,
            Key::Tab => out.tabbed = Some(!shift),
            Key::Char('a') if m.command() => (f.anchor, f.cursor) = (0, f.len()),
            Key::Char('c') if m.command() && !look.password => {
                if let Some(t) = f.selected_text() {
                    fx.out.copy = Some(t);
                }
            }
            Key::Char('x') if m.command() && !look.password && edit => {
                if let Some(t) = f.selected_text() {
                    fx.out.copy = Some(t);
                    f.delete_selection();
                    out.changed = true;
                }
            }
            _ => {}
        }
    }
}
