//! Text for the node view: what to lay out (runs of styled text, a wrap
//! width), and what came back (rows, and where each character is), so the
//! view can measure, paint, hit-test and edit text with any renderer's
//! shaper behind it.

use std::any::Any;
use std::ops::Range;
use std::sync::Arc;

use super::geom::{Pos, Rect, Vec2, pos, vec2};
use crate::node::{Rgba, TextAlign};

/// A font family the view asks for. The faces are the proportional font's
/// (when it has them); `Named` is one Lua registered (`bone.gui.font`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    SansBold,
    /// Weight 500 of the sans family.
    SansMedium,
    SansItalic,
    Mono,
    Named(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunStyle {
    pub family: Family,
    pub size: f32,
    pub color: Rgba,
    /// Slanted by the renderer (a face without an italic of its own).
    pub italic: bool,
    pub background: Option<Rgba>,
    pub underline: bool,
    pub strike: bool,
    /// Pixels from one line to the next (none: the font's own).
    pub line_height: Option<f32>,
    pub letter_spacing: f32,
}

impl RunStyle {
    pub fn plain(family: Family, size: f32, color: Rgba) -> Self {
        RunStyle {
            family,
            size,
            color,
            italic: false,
            background: None,
            underline: false,
            strike: false,
            line_height: None,
            letter_spacing: 0.0,
        }
    }
}

/// Text to lay out: `text` in runs (byte ranges that cover it in order),
/// wrapped at `wrap` (one line that ends in "…" with `ellipsis`), and
/// aligned in a box of `box_width` (else the wrap width).
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub text: String,
    pub runs: Vec<(Range<usize>, RunStyle)>,
    pub wrap: Option<f32>,
    pub ellipsis: bool,
    pub align: TextAlign,
    pub box_width: Option<f32>,
}

impl Job {
    /// The width rows are aligned in, if they are.
    pub fn align_width(&self) -> Option<f32> {
        (self.align != TextAlign::Left)
            .then(|| self.box_width.or(self.wrap))
            .flatten()
    }
}

impl Job {
    pub fn simple(text: String, style: RunStyle, wrap: Option<f32>) -> Job {
        let len = text.len();
        Job {
            text,
            runs: vec![(0..len, style)],
            wrap,
            ellipsis: false,
            align: TextAlign::Left,
            box_width: None,
        }
    }

    pub fn append(&mut self, text: &str, style: RunStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        // Runs of the same style join, as they would be drawn alike.
        if let Some((r, s)) = self.runs.last_mut()
            && *s == style
            && r.end == start
        {
            r.end = self.text.len();
            return;
        }
        self.runs.push((start..self.text.len(), style));
    }

    /// The tallest size in it, which a row is laid out at.
    pub fn max_size(&self) -> f32 {
        (self.runs.iter())
            .map(|(_, s)| s.size)
            .fold(0.0, f32::max)
    }
}

/// One laid-out row of text.
#[derive(Clone, Debug, Default)]
pub struct Row {
    /// Its top and height, from the text's top.
    pub y: f32,
    pub height: f32,
    /// The first character on it (an index in chars, not bytes).
    pub start: usize,
    /// The x of each character boundary on it from the text's left (one
    /// more than its characters: the last is where it ends).
    pub xs: Vec<f32>,
    /// It ends at a newline, which is one character more than `xs` has.
    pub newline: bool,
}

impl Row {
    /// How many characters are drawn on it.
    pub fn chars(&self) -> usize {
        self.xs.len().saturating_sub(1)
    }

    /// Where its characters end, a newline counted.
    pub fn end(&self) -> usize {
        self.start + self.chars() + usize::from(self.newline)
    }

    pub fn left(&self) -> f32 {
        self.xs.first().copied().unwrap_or(0.0)
    }

    pub fn right(&self) -> f32 {
        self.xs.last().copied().unwrap_or(0.0)
    }
}

/// Text laid out by a [`Shaper`]: its size, rows, and the renderer's own
/// form of it for painting.
pub struct Shaped {
    pub text: String,
    pub size: Vec2,
    pub rows: Vec<Row>,
    /// What the renderer paints from (its glyphs or galley).
    pub handle: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for Shaped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shaped")
            .field("text", &self.text)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Shaped {
    /// The box it covers, drawn with its top left at `at`.
    pub fn rect_at(&self, at: Pos) -> Rect {
        Rect::from_min_size(at, self.size)
    }

    /// How many characters it has.
    pub fn char_count(&self) -> usize {
        self.rows.last().map_or(0, Row::end)
    }

    /// The row character `index` is on (the end of a wrapped row is the
    /// start of the next).
    pub fn row_of(&self, index: usize) -> usize {
        let last = self.rows.len().saturating_sub(1);
        for (i, r) in self.rows.iter().enumerate() {
            if index < r.start + r.chars() || (index == r.start + r.chars() && (r.newline || i == last)) {
                return i;
            }
        }
        last
    }

    /// Where a cursor before character `index` is drawn: the top of the
    /// line, from the text's top left, and the line's height.
    pub fn cursor_pos(&self, index: usize) -> (Pos, f32) {
        let Some(row) = self.rows.get(self.row_of(index)) else {
            return (Pos::ZERO, 0.0);
        };
        let col = index.saturating_sub(row.start).min(row.chars());
        let x = row.xs.get(col).copied().unwrap_or(row.right());
        (pos(x, row.y), row.height)
    }

    /// The character boundary nearest `p` (from the text's top left).
    pub fn index_at(&self, p: Pos) -> usize {
        let Some(row) = (self.rows.iter())
            .find(|r| p.y < r.y + r.height)
            .or(self.rows.last())
        else {
            return 0;
        };
        let mut best = 0;
        let mut best_d = f32::INFINITY;
        for (i, x) in row.xs.iter().enumerate() {
            let d = (x - p.x).abs();
            if d < best_d {
                (best, best_d) = (i, d);
            }
        }
        row.start + best
    }

    /// The rectangles covering characters `a..b` (from the text's top
    /// left), a row each.
    pub fn range_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b), a.max(b));
        let mut out = Vec::new();
        for r in &self.rows {
            let (lo, hi) = (r.start, r.start + r.chars());
            if b < lo || a > hi || (a == b) {
                continue;
            }
            let from = a.max(lo) - lo;
            let to = b.min(hi) - lo;
            let x0 = r.xs.get(from).copied().unwrap_or(r.right());
            let mut x1 = r.xs.get(to).copied().unwrap_or(r.right());
            // A selected newline shows as a little room at the line's end.
            if r.newline && b > hi {
                x1 += r.height * 0.3;
            }
            if x1 > x0 {
                out.push(Rect::from_min_size(pos(x0, r.y), vec2(x1 - x0, r.height)));
            }
        }
        out
    }
}

/// What the view lays text out with: the renderer's fonts.
pub trait Shaper {
    fn shape(&self, job: Job) -> Arc<Shaped>;
    /// The height of a line of `family` at `size`.
    fn row_height(&self, family: &Family, size: f32) -> f32;
    /// A character cell of the monospace font at `size`: its advance and
    /// line height.
    fn mono_cell(&self, size: f32) -> (f32, f32);
}

/// The fonts the window should use: `bone.o.font` (the monospace file),
/// `font_size`, the proportional faces, fallbacks for symbols, and the
/// families Lua registered, by name and file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontSet {
    pub mono: Option<String>,
    pub mono_size: f32,
    /// Regular, bold and italic sans files (the options' or the system's).
    pub sans: [Option<String>; 3],
    pub symbols: Vec<String>,
    pub families: Vec<(String, String)>,
}

/// What applying a [`FontSet`] came to.
#[derive(Clone, Debug, Default)]
pub struct FontsApplied {
    /// The sans font has a bold and an italic face of its own.
    pub faces: (bool, bool),
    /// Files that could not be read.
    pub errors: Vec<String>,
}

/// Lay out `job` by fixed advances, for a window-less frame: each char a
/// cell (`size` × 0.6 wide, `size` × 1.25 tall). Wraps at spaces where it
/// can, else anywhere.
pub fn fixed_layout(job: &Job) -> (Vec2, Vec<Row>) {
    fixed_layout_by(job, |_, _| None)
}

/// [`fixed_layout`] with the advance of some chars known: `advance` gives
/// a char's width in ems in a run's style, or `None` for the fixed 0.6.
pub fn fixed_layout_by(job: &Job, advance: impl Fn(&RunStyle, char) -> Option<f32>) -> (Vec2, Vec<Row>) {
    let chars: Vec<(char, f32, f32)> = {
        let mut out = Vec::new();
        for (range, style) in &job.runs {
            for c in job.text[range.clone()].chars() {
                let w = if c == '\n' {
                    0.0
                } else {
                    style.size * advance(style, c).unwrap_or(0.6) + style.letter_spacing
                };
                out.push((c, w, style.line_height.unwrap_or(style.size * 1.25)));
            }
        }
        out
    };
    let base_h = job.max_size().max(1.0) * 1.25;
    let max_w = job.wrap.unwrap_or(f32::INFINITY).max(1.0);
    let mut rows: Vec<Row> = Vec::new();
    let mut row = Row {
        y: 0.0,
        height: 0.0,
        start: 0,
        xs: vec![0.0],
        newline: false,
    };
    let mut x = 0.0;
    let mut y = 0.0;
    let mut i = 0;
    let mut last_space: Option<usize> = None;
    let finish = |row: &mut Row, rows: &mut Vec<Row>, y: &mut f32, next_start: usize| {
        if row.height == 0.0 {
            row.height = base_h;
        }
        *y += row.height;
        let done = std::mem::replace(
            row,
            Row {
                y: *y,
                height: 0.0,
                start: next_start,
                xs: vec![0.0],
                newline: false,
            },
        );
        rows.push(done);
    };
    while i < chars.len() {
        let (c, w, h) = chars[i];
        if c == '\n' {
            row.newline = true;
            row.height = row.height.max(h);
            finish(&mut row, &mut rows, &mut y, i + 1);
            x = 0.0;
            last_space = None;
            i += 1;
            continue;
        }
        if x + w > max_w && row.chars() > 0 && !job.ellipsis {
            // Back to after the last space, if the row has one.
            if let Some(s) = last_space.filter(|s| *s > row.start) {
                let keep = s + 1 - row.start;
                row.xs.truncate(keep + 1);
                finish(&mut row, &mut rows, &mut y, s + 1);
                i = s + 1;
            } else {
                finish(&mut row, &mut rows, &mut y, i);
            }
            x = 0.0;
            last_space = None;
            continue;
        }
        if job.ellipsis && x + w > max_w {
            break;
        }
        if c == ' ' {
            last_space = Some(i);
        }
        x += w;
        row.xs.push(x);
        row.height = row.height.max(h);
        i += 1;
    }
    if row.height == 0.0 {
        row.height = base_h;
    }
    rows.push(row);
    let width = rows.iter().map(Row::right).fold(0.0, f32::max);
    let box_w = job.align_width().unwrap_or(width);
    for r in &mut rows {
        let shift = match job.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (box_w - r.right()) / 2.0,
            TextAlign::Right => box_w - r.right(),
        };
        for x in &mut r.xs {
            *x += shift;
        }
    }
    let height = rows.iter().map(|r| r.height).sum();
    (vec2(width, height), rows)
}

/// The fixed-advance [`Shaper`] of window-less frames.
pub struct FixedShaper;

impl Shaper for FixedShaper {
    fn shape(&self, job: Job) -> Arc<Shaped> {
        let (size, rows) = fixed_layout(&job);
        Arc::new(Shaped {
            text: job.text.clone(),
            size,
            rows,
            handle: Box::new(job),
        })
    }

    fn row_height(&self, _family: &Family, size: f32) -> f32 {
        size * 1.25
    }

    fn mono_cell(&self, size: f32) -> (f32, f32) {
        (size * 0.6, size * 1.25)
    }
}
