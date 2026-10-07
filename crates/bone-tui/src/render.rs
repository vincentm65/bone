//! Drawing. Reads the app state and paints a frame; ratatui diffs frames so
//! only changed cells reach the terminal.
//!
//! Rows come from `bone.ui.layout` (default below); a message line goes
//! under them while there is one.
//!
//! ```text
//! top             region, if defined
//! chat            the current session ("left"/"right" columns beside it,
//!                 then docked Lua panels: bone.ui.panel)
//! divider         only if bone.ui.divider is defined
//! above_prompt    region, if defined
//! prompt          grows with its text; bone.ui.prompt draws the box
//! statusline      only if bone.ui.statusline is defined
//! ```
//! Lua windows (the `/` command menu is one) are drawn on top.

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
use crate::ui::{Border, Item, LayoutNode, PromptSpec, Size, render_items};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    // Consume due UI timers before Lua draws, so render callbacks can rearm
    // them. Keep future deadlines across intervening event-driven frames.
    if app
        .ui_expiry
        .is_some_and(|at| at <= std::time::Instant::now())
    {
        app.ui_expiry = None;
    }
    let area = frame.area();
    app.screen = area;
    app.spinner = app.read_spinner();
    // The Normal group's background (if any) fills the screen.
    frame.render_widget(Block::default().style(app.theme.hl("Normal")), area);
    // Screen-height sidebars reserve a column for the entire main UI.
    let area = app.draw_panels(frame, area, true);

    let layout = app.layout();
    // Without a "message" leaf, notifications take rows at the bottom.
    let msg_rows = if layout.contains("message") {
        0
    } else {
        message_rows(app, area)
    };
    let main = Rect {
        height: area.height.saturating_sub(msg_rows),
        ..area
    };
    let mut message = Rect {
        y: main.bottom(),
        height: msg_rows.min(area.height),
        ..area
    };

    let mut plan = Plan::default();
    place(app, &layout, main, &mut plan);
    let none = Rect { height: 0, ..main };
    let rect = |name: &str| {
        plan.leaves
            .iter()
            .find(|(n, _)| n == name)
            .map_or(none, |(_, r)| *r)
    };
    let middle = rect("chat");
    let divider = rect("divider");
    let prompt_area = rect("prompt");
    let status = rect("statusline");
    if layout.contains("message") {
        message = rect("message");
    }

    // For mouse events: which leaf is where.
    app.leaves = plan.leaves.clone();
    let chat_area = middle;
    let chat_area = app.draw_panels(frame, chat_area, false);
    app.placed = HashMap::from([
        (CHAT_WIN, Placed { area: chat_area }),
        (PROMPT_WIN, Placed { area: prompt_area }),
    ]);
    let was_following = app.windows[&CHAT_WIN].follow;
    draw_chat(frame, app, chat_area);
    let resumed_follow = !was_following && app.windows[&CHAT_WIN].follow;
    for (name, r, lines) in std::mem::take(&mut plan.regions) {
        // Cached lines were made while sizing the layout, before the chat
        // clamped its scroll position. Refresh them if it reached the end.
        let lines = match lines.filter(|_| !resumed_follow) {
            Some(lines) => lines,
            // Not sized by its content: drawn at the size it was given.
            None => match app.region_sized(&name, r.width, r.height, true, true) {
                Some((_, lines)) => lines,
                None => continue,
            },
        };
        frame.render_widget(Paragraph::new(lines), r);
    }
    for (x, area, sep) in std::mem::take(&mut plan.seps) {
        let style = app.theme.hl("WinSeparator");
        for y in area.y..area.bottom() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_symbol(&sep).set_style(style);
            }
        }
    }
    let (spec, insets, prompt_ctx) = match plan.prompt.take() {
        Some(p) => p,
        None => {
            let ctx = app.prompt_ctx(main.width);
            let spec = PromptSpec::default();
            let insets = Insets::of(&spec, main.width);
            (spec, insets, ctx)
        }
    };

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
        draw_prompt(frame, app, prompt_area, &spec, &insets, prompt_ctx)
    } else {
        None
    };
    draw_status(frame, app, status);
    draw_message(frame, app, message);

    if !app.popups.is_empty() {
        let prompt_top = if prompt_area.height > 0 {
            prompt_area.y
        } else {
            main.bottom()
        };
        let anchors = Anchors {
            screen: app.screen,
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
    // A finished selection is copied, unless a `select` handler returns
    // true for its text.
    let finished = match &mut app.selection {
        Some(s) if s.done && !s.copied => {
            s.copied = true;
            Some(s.text(frame.buffer_mut()))
        }
        _ => None,
    };
    if let Some(text) = finished {
        let taken = app
            .fire("select", serde_json::json!({ "text": text }))
            .iter()
            .any(|v| matches!(v, mlua::Value::Boolean(true)));
        if !taken {
            app.clipboard = Some(text);
        }
    }
    if let Some(s) = &mut app.selection {
        s.highlight(frame.buffer_mut());
    }
}

/// Where the layout put things.
#[derive(Default)]
struct Plan {
    /// Every leaf and its area.
    leaves: Vec<(String, Rect)>,
    /// Regions to draw: lines already made when sized by content.
    regions: Vec<(String, Rect, Option<Vec<Line<'static>>>)>,
    /// Column separators: x, the split's area, the symbol.
    seps: Vec<(u16, Rect, String)>,
    prompt: Option<(PromptSpec, Insets, serde_json::Value)>,
}

const BUILTIN: [&str; 5] = ["chat", "prompt", "divider", "statusline", "message"];

/// Lay `node` out in `area`. In a split, fixed and percent sizes come first,
/// then natural sizes (the prompt, one-row statusline and divider, the
/// message line, then regions, which leave a filling sibling at least 3
/// rows), and filling children share the rest.
fn place(app: &mut App, node: &LayoutNode, area: Rect, plan: &mut Plan) {
    let LayoutNode::Split {
        rows,
        sep,
        children,
        ..
    } = node
    else {
        if let LayoutNode::Leaf { name, .. } = node {
            plan.leaves.push((name.clone(), area));
            if !BUILTIN.contains(&name.as_str()) {
                plan.regions.push((name.clone(), area, None));
            }
        }
        return;
    };
    let rows = *rows;
    let n = children.len();
    // Separators go only between columns that have something in them; room
    // for the most there could be is kept back while sizing regions.
    let sep = sep.as_ref().filter(|_| !rows);
    let most_seps = if sep.is_some() {
        n.saturating_sub(1) as u16
    } else {
        0
    };
    let total = if rows { area.height } else { area.width };
    let cross = if rows { area.width } else { area.height };
    let mut sizes: Vec<Option<u16>> = vec![None; n];
    let mut lines: Vec<Option<Vec<Line<'static>>>> = vec![None; n];
    let leaf = |c: &LayoutNode| match c {
        LayoutNode::Leaf { name, .. } => Some(name.clone()),
        _ => None,
    };
    for (i, c) in children.iter().enumerate() {
        sizes[i] = match (c.size(), leaf(c).as_deref()) {
            (Size::Cells(v), _) => Some(v),
            (Size::Percent(p), _) => Some((u32::from(total) * u32::from(p) / 100) as u16),
            (Size::Auto, Some("prompt")) => {
                let ctx = app.prompt_ctx(if rows { cross } else { total });
                let spec = app.prompt_spec(&ctx);
                let width = if rows { cross } else { total };
                let insets = Insets::of(&spec, width);
                let text = prompt_rows(&app.prompt, insets.text_width).0.len();
                let max = spec
                    .max_rows
                    .unwrap_or(app.options.prompt_max_height)
                    .max(spec.min_rows);
                let h = text.clamp(spec.min_rows, max) as u16 + insets.top + insets.bottom;
                plan.prompt = Some((spec, insets, ctx));
                Some(if rows {
                    h.min(total.saturating_sub(1))
                } else {
                    total / 2
                })
            }
            (Size::Auto, Some(name @ ("divider" | "statusline"))) => {
                Some(u16::from(app.ui_defined(name)))
            }
            (Size::Auto, Some("message")) => Some(message_rows(app, area)),
            _ => None,
        };
    }
    // The prompt also needs its spec when it was given a fixed size.
    for (i, c) in children.iter().enumerate() {
        if leaf(c).as_deref() == Some("prompt") && plan.prompt.is_none() {
            let width = if rows {
                cross
            } else {
                sizes[i].unwrap_or(total)
            };
            let ctx = app.prompt_ctx(width);
            let spec = app.prompt_spec(&ctx);
            let insets = Insets::of(&spec, width);
            plan.prompt = Some((spec, insets, ctx));
        }
    }
    let fills = children
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            sizes[*i].is_none()
                && !(c.size() == Size::Auto
                    && leaf(c).is_some_and(|n| !BUILTIN.contains(&n.as_str())))
        })
        .count();
    let used: u16 = sizes.iter().flatten().sum();
    let keep = if fills > 0 && rows { 3 } else { 0 };
    let mut spare = total.saturating_sub(used + keep + most_seps);
    for (i, c) in children.iter().enumerate() {
        if sizes[i].is_some() || c.size() != Size::Auto {
            continue;
        }
        let Some(name) = leaf(c).filter(|n| !BUILTIN.contains(&n.as_str())) else {
            continue;
        };
        let (avail_w, avail_h) = if rows {
            (cross, spare)
        } else {
            (spare.max(total), cross)
        };
        sizes[i] = Some(
            match app.region_sized(&name, avail_w, avail_h, rows, false) {
                Some((size, l)) => {
                    let size = size.min(spare);
                    lines[i] = Some(l);
                    size
                }
                None => 0,
            },
        );
        spare = spare.saturating_sub(sizes[i].unwrap_or(0));
    }
    let used: u16 = sizes.iter().flatten().sum();
    let fill_count = sizes.iter().filter(|s| s.is_none()).count() as u16;
    let shown = sizes.iter().filter(|s| s.is_none_or(|v| v > 0)).count() as u16;
    let seps = if sep.is_some() {
        shown.saturating_sub(1)
    } else {
        0
    };
    let left = total.saturating_sub(used + seps);
    let mut extra = if fill_count > 0 { left % fill_count } else { 0 };
    let mut at = if rows { area.y } else { area.x };
    let end = if rows { area.bottom() } else { area.right() };
    let mut shown_before = false;
    for (i, c) in children.iter().enumerate() {
        let mut size = sizes[i].unwrap_or_else(|| {
            let share = left / fill_count.max(1) + u16::from(extra > 0);
            extra = extra.saturating_sub(1);
            share
        });
        if let Some(sep) = sep
            && size > 0
            && shown_before
            && at < end
        {
            plan.seps.push((at, area, sep.clone()));
            at += 1;
        }
        shown_before |= size > 0;
        size = size.min(end.saturating_sub(at));
        let r = if rows {
            Rect {
                y: at,
                height: size,
                ..area
            }
        } else {
            Rect {
                x: at,
                width: size,
                ..area
            }
        };
        match c {
            LayoutNode::Split { .. } => place(app, c, r, plan),
            LayoutNode::Leaf { name, .. } => {
                plan.leaves.push((name.clone(), r));
                if !BUILTIN.contains(&name.as_str()) {
                    plan.regions.push((name.clone(), r, lines[i].take()));
                }
            }
        }
        at += size;
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
        // Regions were sized before the chat reached the end. Request
        // another layout pass in case their size depends on following.
        app.redraw |= !win.follow;
        win.follow = true;
    }
    let lines: Vec<Line> = chat.rows().skip(win.top).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(lines), area);
    if total == 0 {
        if let Some((_, lines)) =
            app.region_sized("chat_empty", area.width, area.height, true, true)
        {
            let height = (lines.len().min(area.height as usize)) as u16;
            let centered = Rect {
                y: area.y + area.height.saturating_sub(height) / 2,
                height,
                ..area
            };
            frame.render_widget(
                Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
                centered,
            );
        }
    }
}

/// What the prompt box takes around its text, and the text's width.
struct Insets {
    /// Rows above the text: the top edge (border or `top` line), padding.
    top: u16,
    bottom: u16,
    /// Columns beside it: border, padding.
    left: u16,
    right: u16,
    /// Columns before each text row: the prefix (or continuation).
    gutter: usize,
    text_width: usize,
}

impl Insets {
    fn of(spec: &PromptSpec, width_cols: u16) -> Self {
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

fn items_width(items: &[Item]) -> usize {
    items
        .iter()
        .map(|i| match i {
            Item::Text(t, _) => width(t),
            Item::Fill(..) => 0,
        })
        .sum()
}

/// Draw the prompt box: background, border and edge lines from
/// `bone.ui.prompt`, then the text (wrapped, scrolled and selected here, so
/// the cursor lands exactly). Returns where the cursor goes.
fn draw_prompt(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    spec: &PromptSpec,
    insets: &Insets,
    mut ctx: serde_json::Value,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let inner = Rect {
        x: area.x + insets.left.min(area.width),
        y: area.y + insets.top.min(area.height),
        width: area.width.saturating_sub(insets.left + insets.right),
        height: area.height.saturating_sub(insets.top + insets.bottom),
    };
    let gutter = insets.gutter;
    let w = (inner.width as usize).saturating_sub(gutter).max(1);
    let (rows, (crow, ccol)) = prompt_layout(&app.prompt, w);
    let height = inner.height as usize;
    let win = app.windows.get_mut(&PROMPT_WIN).unwrap();
    // Keep the cursor row visible.
    if crow < win.top {
        win.top = crow;
    } else if height > 0 && crow >= win.top + height {
        win.top = crow + 1 - height;
    }
    let top = win.top;

    // The edges get the laid-out box too.
    let border_hl = spec
        .border
        .as_ref()
        .map_or("PromptBorder", |b| b.hl.as_str())
        .to_owned();
    ctx["rows"] = rows.len().into();
    ctx["height"] = height.into();
    ctx["text_width"] = w.into();
    ctx["scroll"] = top.into();
    ctx["cursor"]["screen"] = serde_json::json!({ "row": crow, "col": ccol });
    let top_line = spec
        .has_top
        .then(|| app.prompt_edge("top", &ctx, &border_hl));
    let bottom_line = spec
        .has_bottom
        .then(|| app.prompt_edge("bottom", &ctx, &border_hl));

    let theme = &app.theme;
    let fill = spec.background.as_deref().map(|bg| theme.hl(bg));
    if let Some(style) = fill {
        frame.render_widget(Block::default().style(style), area);
    }
    let border = spec.border.as_ref();
    let top_row = border.is_some_and(|b| b.top) || spec.has_top;
    let bottom_row = border.is_some_and(|b| b.bottom) || spec.has_bottom;
    if top_row {
        let row = Rect { height: 1, ..area };
        draw_edge(frame, theme, row, border, true, top_line.as_deref());
    }
    if bottom_row && area.height > 1 {
        let row = Rect {
            y: area.bottom() - 1,
            height: 1,
            ..area
        };
        draw_edge(frame, theme, row, border, false, bottom_line.as_deref());
    }
    if let Some(b) = border {
        let style = theme.hl(&b.hl);
        let from = area.y + u16::from(top_row);
        let to = area.bottom().saturating_sub(u16::from(bottom_row));
        for y in from..to {
            for (on, x) in [(b.left, area.x), (b.right, area.right() - 1)] {
                if let (true, Some(cell)) = (on, frame.buffer_mut().cell_mut((x, y))) {
                    cell.set_char(b.chars[5]).set_style(style);
                }
            }
        }
    }
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let marks = if app.prompt.is_empty() {
        Default::default()
    } else {
        app.prompt_highlight(&ctx)
    };
    let theme = &app.theme;
    let cursor_line = app.prompt.cursor();
    let at_line_end = app
        .prompt
        .lines()
        .get(cursor_line.0)
        .is_some_and(|l| cursor_line.1 >= l.chars().count());
    let selection = app.prompt.selection();
    let selected = theme.hl("Selection");
    // A row's gutter: the prefix on the first, the continuation (or blanks
    // as wide) after; both padded to the same width.
    let gutter_spans = |items: Option<&[Item]>| -> Vec<Span<'static>> {
        let mut spans = match items {
            Some(i) => render_items(i, gutter, theme).spans,
            None => Vec::new(),
        };
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        if used < gutter {
            spans.push(Span::raw(" ".repeat(gutter - used)));
        }
        spans
    };

    let lines: Vec<Line> = if app.prompt.is_empty() {
        let mut spans = gutter_spans(Some(&spec.prefix));
        spans.extend(render_items(&spec.placeholder, w, theme).spans);
        vec![Line::from(spans)]
    } else {
        rows.iter()
            .enumerate()
            .skip(top)
            .take(height)
            .map(|(i, r)| {
                let mut spans = if i == 0 {
                    gutter_spans(Some(&spec.prefix))
                } else {
                    gutter_spans(spec.continuation.as_deref())
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
                // Each char's style: Lua's marks, then the selection.
                let chars: Vec<char> = text.chars().collect();
                let mut styles = vec![ratatui::style::Style::default(); chars.len()];
                for m in marks.ranges.iter().filter(|m| m.row == *line) {
                    let style = theme.hl(&m.hl);
                    for (k, s) in styles.iter_mut().enumerate() {
                        if (m.from..m.to).contains(&(start + k)) {
                            *s = style;
                        }
                    }
                }
                if let Some((lo, hi)) = range {
                    for s in styles.iter_mut().take(hi).skip(lo) {
                        *s = selected;
                    }
                }
                let mut k = 0;
                while k < chars.len() {
                    let mut j = k + 1;
                    while j < chars.len() && styles[j] == styles[k] {
                        j += 1;
                    }
                    spans.push(Span::styled(
                        chars[k..j].iter().collect::<String>(),
                        styles[k],
                    ));
                    k = j;
                }
                // Ghost text after the cursor, at the end of its line.
                if i == crow
                    && at_line_end
                    && let Some(ghost) = marks.ghost.as_deref().filter(|g| !g.is_empty())
                {
                    spans.push(Span::styled(
                        ghost.lines().next().unwrap_or("").to_owned(),
                        theme.hl(&marks.ghost_hl),
                    ));
                }
                Line::from(spans)
            })
            .collect()
    };
    let mut text = Paragraph::new(lines);
    if let Some(style) = fill {
        text = text.style(style);
    }
    frame.render_widget(text, inner);
    Some(Position {
        x: inner.x + (gutter + ccol).min(inner.width as usize - 1) as u16,
        y: inner.y + (crow - top) as u16,
    })
}

/// The top (or bottom) row of the prompt box: corners where the border
/// has both sides, and between them the edge's line, or the border.
fn draw_edge(
    frame: &mut Frame<'_>,
    theme: &crate::theme::Theme,
    row: Rect,
    border: Option<&Border>,
    top: bool,
    line: Option<&[Item]>,
) {
    let mut mid = row;
    if let Some(b) = border {
        let on = if top { b.top } else { b.bottom };
        let style = theme.hl(&b.hl);
        let (lc, rc) = if top {
            (b.chars[0], b.chars[1])
        } else {
            (b.chars[2], b.chars[3])
        };
        let buf = frame.buffer_mut();
        if on && b.left && mid.width > 0 {
            if let Some(c) = buf.cell_mut((mid.x, mid.y)) {
                c.set_char(lc).set_style(style);
            }
            mid.x += 1;
            mid.width -= 1;
        }
        if on && b.right && mid.width > 0 {
            if let Some(c) = buf.cell_mut((mid.right() - 1, mid.y)) {
                c.set_char(rc).set_style(style);
            }
            mid.width -= 1;
        }
        if on && line.is_none_or(<[Item]>::is_empty) {
            for x in mid.x..mid.right() {
                if let Some(c) = buf.cell_mut((x, mid.y)) {
                    c.set_char(b.chars[4]).set_style(style);
                }
            }
            return;
        }
    }
    if let Some(items) = line.filter(|l| !l.is_empty()) {
        let l = render_items(items, mid.width as usize, theme);
        frame.render_widget(Paragraph::new(l), mid);
    }
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

    #[tokio::test]
    async fn panel_refresh_timer_preserves_chat_cache_and_rearms() {
        let (conn, _server) = bone_proto::transport::in_process();
        let (client, _events) = bone_client::Client::new(conn);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(std::sync::Arc::new(client), tx, "/work".into(), None);
        app.with_api(|lua| {
            lua.load(
                r#"
                chat_ticks = 0
                panel_ticks = 0
                bone.ui.views.timer_test = function(item)
                  chat_ticks = chat_ticks + 1
                  return { "cached chat" }
                end
                bone.chat.add("timer_test", { text = "cached chat" })
                bone.ui.panel.open({
                  id = "timer", dock = "right", size = 20,
                  render = function(ctx)
                    panel_ticks = panel_ticks + 1
                    bone.ui.refresh_in(10000)
                    return { "panel tick " .. panel_ticks }
                  end,
                })
                "#,
            )
            .exec()
        })
        .unwrap();
        let rev = app.views_rev;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let first = app.ui_expiry.unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.ui_expiry, Some(first));
        app.ui_expiry = Some(std::time::Instant::now());
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.ui_expiry.unwrap() > first);
        assert_eq!(app.views_rev, rev);
        assert!(app.chat_expiry.is_none());
        let ticks: (usize, usize) = app
            .with_api(|lua| lua.load("return chat_ticks, panel_ticks").eval())
            .unwrap();
        assert_eq!(ticks, (1, 3));
        // Multiple callers retain the earliest deadline, including an immediate one.
        app.with_api(|lua| {
            lua.load("bone.ui.refresh_in(-1); bone.ui.refresh_in(20000)")
                .exec()
        })
        .unwrap();
        assert!(app.ui_expiry.unwrap() <= std::time::Instant::now());
    }
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
