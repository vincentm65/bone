//! Drawing. Reads the app state and paints a frame; ratatui diffs frames so
//! only changed cells reach the terminal.
//!
//! Rows come from `bone.ui.layout` (default below); a message line goes
//! under them while there is one.
//!
//! ```text
//! top             region, if defined
//! chat            the current session ("left"/"right" regions beside it,
//!                 then docked Lua panels: bone.ui.panel)
//! divider         only if bone.ui.divider is defined
//! above_prompt    region, if defined
//! prompt          grows with its text; bone.ui.prompt adds a prefix
//! statusline      only if bone.ui.statusline is defined
//! ```
//! Slash-command suggestions (`bone.ui.suggestions`) and Lua windows are
//! drawn on top.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, CHAT_WIN, Level, PROMPT_WIN};
use crate::editor::TextBuffer;
use crate::layout::Placed;
use crate::text::{sanitize, width};
use crate::ui::{Item, render_items};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    app.screen = area;
    // The Normal group's background (if any) fills the screen.
    frame.render_widget(Block::default().style(app.theme.hl("Normal")), area);

    let msg_rows = message_rows(app, area);
    let layout = app.layout();
    let has = |n: &str| layout.iter().any(|l| l == n);
    let status_h = u16::from(has("statusline") && app.ui_defined("statusline"));
    let divider_h = u16::from(has("divider") && app.ui_defined("divider"));
    let (prefix, placeholder) = app.prompt_decor();
    let gutter = prefix
        .iter()
        .map(|i| match i {
            Item::Text(t, _) => width(t),
            Item::Fill(..) => 0,
        })
        .sum::<usize>();
    let main = Rect {
        height: area.height.saturating_sub(msg_rows),
        ..area
    };
    let message = Rect {
        y: main.bottom(),
        height: msg_rows.min(area.height),
        ..area
    };

    // Rows in `bone.ui.layout` order. Fixed rows first, then regions (never
    // squeezing the chat below a few rows), then the chat takes the rest.
    let prompt_h = if has("prompt") {
        let rows = prompt_rows(&app.prompt, (main.width as usize).saturating_sub(gutter))
            .0
            .len();
        (rows.clamp(1, app.options.prompt_max_height) as u16)
            .min(main.height.saturating_sub(1 + divider_h + status_h))
    } else {
        0
    };
    let mut spare = main
        .height
        .saturating_sub(prompt_h + divider_h + status_h + 3);
    let mut regions: HashMap<String, Vec<Line<'static>>> = HashMap::new();
    let mut heights: Vec<u16> = Vec::new();
    for name in &layout {
        let h = match name.as_str() {
            "chat" => 0,
            "prompt" => prompt_h,
            "divider" => divider_h,
            "statusline" => status_h,
            region => match app.region(region, main.width, spare) {
                Some((h, lines)) => {
                    spare = spare.saturating_sub(h);
                    regions.insert(region.to_owned(), lines);
                    h
                }
                None => 0,
            },
        };
        heights.push(h);
    }
    let chat_h = main.height.saturating_sub(heights.iter().sum());
    let mut y = main.y;
    let mut rects: HashMap<&str, Rect> = HashMap::new();
    for (name, h) in layout.iter().zip(heights) {
        let h = if name == "chat" { chat_h } else { h };
        let h = h.min(main.bottom().saturating_sub(y));
        rects.insert(
            name.as_str(),
            Rect {
                y,
                height: h,
                ..main
            },
        );
        y += h;
    }
    let none = Rect { height: 0, ..main };
    let middle = rects.get("chat").copied().unwrap_or(none);
    let divider = rects.get("divider").copied().unwrap_or(none);
    let prompt_area = rects.get("prompt").copied().unwrap_or(none);
    let status = rects.get("statusline").copied().unwrap_or(none);

    let mut chat_area = middle;
    if middle.width >= 40 {
        if let Some((w, lines)) = app.region("left", middle.width, middle.height) {
            let r = Rect { width: w, ..middle };
            frame.render_widget(Paragraph::new(lines), r);
            vertical_bar(frame, app, r.right(), middle);
            chat_area.x += w + 1;
            chat_area.width = chat_area.width.saturating_sub(w + 1);
        }
        if let Some((w, lines)) = app.region("right", chat_area.width, middle.height) {
            let r = Rect {
                x: chat_area.right().saturating_sub(w),
                width: w,
                ..middle
            };
            frame.render_widget(Paragraph::new(lines), r);
            vertical_bar(frame, app, r.x.saturating_sub(1), middle);
            chat_area.width = chat_area.width.saturating_sub(w + 1);
        }
    }
    let chat_area = app.draw_panels(frame, chat_area);
    for (name, lines) in regions {
        if let Some(r) = rects.get(name.as_str()) {
            frame.render_widget(Paragraph::new(lines), *r);
        }
    }

    app.placed = HashMap::from([
        (CHAT_WIN, Placed { area: chat_area }),
        (PROMPT_WIN, Placed { area: prompt_area }),
    ]);
    draw_chat(frame, app, chat_area);
    if divider.height > 0 {
        let ctx = app.divider_ctx(divider.width);
        let items = app
            .ui_items("divider", ctx, "WinSeparator")
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(render_items(&items, divider.width as usize, &app.theme)),
            divider,
        );
    }
    let mut cursor = if prompt_area.height > 0 {
        draw_prompt(frame, app, prompt_area, &prefix, &placeholder)
    } else {
        None
    };
    draw_status(frame, app, status);
    draw_message(frame, app, message);

    let suggestions = app.suggestions();
    if !suggestions.is_empty() && prompt_area.height > 0 {
        draw_suggestions(frame, app, &suggestions, prompt_area);
    }
    if !app.popups.is_empty() {
        let prompt_top = if prompt_area.height > 0 {
            prompt_area.y
        } else {
            main.bottom()
        };
        let anchors = Anchors {
            screen: area,
            chat: chat_area,
            prompt: Rect {
                y: area.y,
                height: prompt_top.saturating_sub(area.y),
                ..area
            },
        };
        draw_popups(frame, app, &anchors);
        if app.focused_popup().is_some() {
            cursor = None;
        }
    }
    if app.focused_panel().is_some() {
        cursor = None;
    }
    if let Some(c) = cursor {
        frame.set_cursor_position(c);
    }
    if let Some(s) = &mut app.selection {
        if s.done && !s.copied {
            s.copied = true;
            app.clipboard = Some(s.text(frame.buffer_mut()));
        }
        s.highlight(frame.buffer_mut());
    }
}

fn vertical_bar(frame: &mut Frame<'_>, app: &App, x: u16, area: Rect) {
    let style = app.theme.hl("WinSeparator");
    for y in area.y..area.bottom() {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_symbol("│").set_style(style);
        }
    }
}

fn message_rows(app: &App, area: Rect) -> u16 {
    match &app.message {
        Some((text, _)) => (text.lines().count().max(1) as u16)
            .min(area.height / 2)
            .max(1),
        None => 0,
    }
}

fn draw_chat(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let height = area.height as usize;
    app.render_chat(app.current, area.width as usize);
    let chat = &app.chats[app.current];
    let total = chat.row_count();
    let win = app.windows.get_mut(&CHAT_WIN).unwrap();
    let max_top = total.saturating_sub(height);
    if win.follow || win.top >= max_top {
        win.top = max_top;
        win.follow = true;
    }
    let lines: Vec<Line> = chat.rows().skip(win.top).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Draw the prompt after `prefix` (continuation rows are indented to
/// match); `placeholder` shows while it is empty. Returns where the cursor
/// goes.
fn draw_prompt(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    prefix: &[Item],
    placeholder: &[Item],
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let height = area.height as usize;
    let theme = &app.theme;
    let prefix = render_items(prefix, area.width as usize, theme);
    let gutter = prefix.width();
    let indent = Span::raw(" ".repeat(gutter));
    let w = (area.width as usize).saturating_sub(gutter).max(1);
    let (rows, (crow, ccol)) = prompt_layout(&app.prompt, w);
    let selection = app.prompt.selection();
    let selected = theme.hl("Selection");
    let win = app.windows.get_mut(&PROMPT_WIN).unwrap();
    // Keep the cursor row visible.
    if crow < win.top {
        win.top = crow;
    } else if crow >= win.top + height {
        win.top = crow + 1 - height;
    }
    let top = win.top;
    let lines: Vec<Line> = if app.prompt.is_empty() {
        let mut spans = prefix.spans.clone();
        spans.extend(render_items(placeholder, w, theme).spans);
        vec![Line::from(spans)]
    } else {
        rows.iter()
            .enumerate()
            .skip(top)
            .take(height)
            .map(|(i, r)| {
                let mut spans = if i == 0 {
                    prefix.spans.clone()
                } else {
                    vec![indent.clone()]
                };
                let (text, line, start) = r;
                // The selected chars of this row, if any.
                let range = selection.map(|(a, b)| {
                    let n = text.chars().count();
                    let at = |p: (usize, usize)| match p.0.cmp(line) {
                        std::cmp::Ordering::Less => 0,
                        std::cmp::Ordering::Greater => n,
                        std::cmp::Ordering::Equal => p.1.saturating_sub(*start).min(n),
                    };
                    (at(a), at(b))
                });
                match range {
                    Some((lo, hi)) if lo < hi => {
                        let part = |from: usize, to: usize| -> String {
                            text.chars().skip(from).take(to - from).collect()
                        };
                        spans.push(Span::raw(part(0, lo)));
                        spans.push(Span::styled(part(lo, hi), selected));
                        spans.push(Span::raw(part(hi, text.chars().count())));
                    }
                    _ => spans.push(Span::raw(text.clone())),
                }
                Line::from(spans)
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), area);
    Some(Position {
        x: area.x + (gutter + ccol).min(area.width as usize - 1) as u16,
        y: area.y + (crow - top) as u16,
    })
}

/// Char-wrap prompt text (so cursor mapping stays exact). Returns the rows and
/// the cursor's (row, column).
pub fn prompt_rows(t: &TextBuffer, width: usize) -> (Vec<String>, (usize, usize)) {
    let (rows, cursor) = prompt_layout(t, width);
    (rows.into_iter().map(|r| r.0).collect(), cursor)
}

/// A wrapped prompt row: its text, and the line and column it starts at.
type PromptRow = (String, usize, usize);

/// `prompt_rows`, with where each row starts in the text.
fn prompt_layout(t: &TextBuffer, width: usize) -> (Vec<PromptRow>, (usize, usize)) {
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

fn draw_status(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let ctx = app.statusline_ctx(area.width);
    let items = app
        .ui_items("statusline", ctx, "StatusLine")
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(render_items(&items, area.width as usize, &app.theme)),
        area,
    );
}

fn draw_message(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if let Some((text, level)) = &app.message {
        let style = if *level == Level::Error {
            app.theme.hl("ErrorMsg")
        } else {
            app.theme.hl("Normal")
        };
        let lines: Vec<Line> = sanitize(text)
            .lines()
            .map(|l| Line::styled(l.to_owned(), style))
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }
}

/// Matching slash commands, drawn by `bone.ui.suggestions` right above the
/// prompt (nothing if it is not defined).
fn draw_suggestions(
    frame: &mut Frame<'_>,
    app: &mut App,
    items: &[(String, String)],
    prompt: Rect,
) {
    let room = Rect {
        x: prompt.x,
        y: frame.area().y,
        width: prompt.width,
        height: prompt.y.saturating_sub(frame.area().y),
    };
    if room.height == 0 {
        return;
    }
    let lines = app.suggestion_lines(items, room.width, room.height);
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
    let h = (lines.len() as u16).min(room.height);
    if w == 0 || h == 0 {
        return;
    }
    let rect = Rect {
        x: room.x,
        y: room.bottom() - h,
        width: w.min(room.width),
        height: h,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).style(app.theme.hl("Normal")), rect);
}

/// The areas windows can be placed in.
struct Anchors {
    screen: Rect,
    chat: Rect,
    /// Everything above the prompt.
    prompt: Rect,
}

/// Lua windows, bottom first. Lua draws every cell inside; Rust sizes,
/// places and clears behind them.
fn draw_popups(frame: &mut Frame<'_>, app: &mut App, anchors: &Anchors) {
    let order: Vec<crate::app::Popup> = app.popups_in_order().into_iter().cloned().collect();
    for p in order {
        let (id, lines_cb, width, height, row, col, anchor) =
            (p.id, p.lines, p.width, p.height, p.row, p.col, p.anchor);
        let area = match anchor.as_str() {
            "chat" => anchors.chat,
            "prompt" => anchors.prompt,
            _ => anchors.screen,
        };
        if area.width == 0 || area.height == 0 {
            continue;
        }
        // Above the prompt, windows sit right on it unless told otherwise.
        let row = row.or((anchor == "prompt").then_some(-1));
        let avail_w = width.unwrap_or(area.width).min(area.width);
        let avail_h = height.unwrap_or(area.height).min(area.height);
        let lines = app.popup_lines(id, lines_cb, avail_w, avail_h);
        let w = width.unwrap_or_else(|| lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16);
        let h = height.unwrap_or(lines.len() as u16);
        let (w, h) = (w.min(area.width), h.min(area.height));
        if w == 0 || h == 0 {
            continue;
        }
        let place = |at: Option<i32>, size: u16, total: u16| -> u16 {
            let free = total - size;
            match at {
                None => free / 2,
                Some(n) if n < 0 => free.saturating_sub((-n - 1) as u16),
                Some(n) => (n as u16).min(free),
            }
        };
        let rect = Rect {
            x: area.x + place(col.or((anchor == "prompt").then_some(0)), w, area.width),
            y: area.y + place(row, h, area.height),
            width: w,
            height: h,
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).style(app.theme.hl("Normal")), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_rows_map_the_cursor() {
        let mut t = TextBuffer::default();
        t.set_text("abcdef\nxy");
        assert_eq!(
            prompt_rows(&t, 4),
            (vec!["abcd".into(), "ef".into(), "xy".into()], (2, 2))
        );
        t.up();
        t.line_end();
        assert_eq!(prompt_rows(&t, 4).1, (1, 2));
        t.set_text("abcd");
        // Cursor after a full row moves to a fresh row.
        assert_eq!(prompt_rows(&t, 4), (vec!["abcd".into(), "".into()], (1, 0)));
    }
}
