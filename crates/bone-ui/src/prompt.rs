//! The prompt box's geometry: what its border, padding and prefix take,
//! how its text wraps, and how tall it wants to be.

use unicode_width::UnicodeWidthChar;

use crate::editor::TextBuffer;
use crate::model::{Border, PromptSpec, items_width};

/// Rows and columns around the prompt's text, and the width left for it.
/// What the prompt box takes around its text, and the text's width.
pub struct Insets {
    /// Rows above the text: the top edge (border or `top` line), padding.
    pub top: u16,
    pub bottom: u16,
    /// Columns beside it: border, padding.
    pub left: u16,
    pub right: u16,
    /// Columns before each text row: the prefix (or continuation).
    pub gutter: usize,
    pub text_width: usize,
}

impl Insets {
    pub fn of(spec: &PromptSpec, width_cols: u16) -> Self {
        let b = spec.border.as_ref();
        let side = |on: fn(&Border) -> bool| u16::from(b.is_some_and(on));
        let top = u16::from(b.is_some_and(|b| b.top) || spec.has_top) + spec.pad_rows;
        let bottom = u16::from(b.is_some_and(|b| b.bottom) || spec.has_bottom) + spec.pad_rows;
        let left = side(|b| b.left) + spec.pad_cols;
        let right = side(|b| b.right) + spec.pad_cols;
        let gutter =
            items_width(&spec.prefix).max(spec.continuation.as_deref().map_or(0, items_width));
        let text_width = (width_cols as usize)
            .saturating_sub((left + right) as usize + gutter)
            .max(1);
        Insets {
            top,
            bottom,
            left,
            right,
            gutter,
            text_width,
        }
    }
}

/// Char-wrap prompt text (so cursor mapping stays exact). Returns the rows and
/// the cursor's (row, column).
pub fn prompt_rows(t: &TextBuffer, width: usize) -> (Vec<String>, (usize, usize)) {
    let (rows, cursor) = prompt_layout(t, width);
    (rows.into_iter().map(|r| r.0).collect(), cursor)
}

/// A wrapped prompt row: its text, and the line and column it starts at.
pub type PromptRow = (String, usize, usize);

/// `prompt_rows`, with where each row starts in the text.
pub fn prompt_layout(t: &TextBuffer, width: usize) -> (Vec<PromptRow>, (usize, usize)) {
    let width = width.max(1);
    let (crow, ccol) = t.cursor();
    let mut rows = Vec::new();
    let mut cursor = (0, 0);
    for (li, line) in t.lines().iter().enumerate() {
        let mut row = String::new();
        let mut start = 0;
        let mut w = 0;
        for (ci, c) in line.chars().enumerate() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width {
                rows.push((std::mem::take(&mut row), li, start));
                start = ci;
                w = 0;
            }
            if li == crow && ci == ccol {
                cursor = (rows.len(), w);
            }
            row.push(c);
            w += cw;
        }
        if li == crow && ccol >= line.chars().count() {
            // Cursor after the last char; wrap if the row is full.
            if w >= width {
                rows.push((std::mem::take(&mut row), li, start));
                start = line.chars().count();
                w = 0;
            }
            cursor = (rows.len(), w);
        }
        rows.push((row, li, start));
    }
    (rows, cursor)
}

/// The prompt's natural height at `width`: its wrapped text between the
/// spec's minimum and maximum rows (`default_max` without one), plus its
/// border and padding.
pub fn natural_height(spec: &PromptSpec, text: &TextBuffer, width: u16, default_max: usize) -> u16 {
    let insets = Insets::of(spec, width);
    let rows = prompt_rows(text, insets.text_width).0.len();
    let max = spec.max_rows.unwrap_or(default_max).max(spec.min_rows);
    (rows.clamp(spec.min_rows, max).min(u16::MAX as usize) as u16)
        .saturating_add(insets.top)
        .saturating_add(insets.bottom)
}
