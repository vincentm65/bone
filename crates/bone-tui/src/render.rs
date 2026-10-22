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

use crate::app::{App, CHAT_WIN, Level, PROMPT_WIN};
#[cfg(test)]
use crate::editor::TextBuffer;
use crate::layout::{BufferId, Placed, Window};
use crate::text::{sanitize, width};
use crate::ui::{Border, Item, LayoutNode, render_items};
use bone_ui::layout::{self, BUILTIN, kind};
#[cfg(test)]
use bone_ui::prompt::prompt_rows;
use bone_ui::prompt::{Insets, prompt_layout};

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
    crate::stats::mark("pre");
    app.spinner = app.read_spinner();
    crate::stats::mark("spinner");
    // The Normal group's background (if any) fills the screen.
    frame.render_widget(Block::default().style(app.theme.hl("Normal")), area);
    // Screen-height sidebars reserve a column for the entire main UI.
    let layout = app.layout();
    crate::stats::mark("layout");
    let mut panels = Vec::new();
    let area = app.allocate_panels(area, true, &layout, &mut panels);
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

    let mut plan = place(app, &layout, main);
    crate::stats::mark("place");
    let none = Rect { height: 0, ..main };
    let rect = |leaves: &[(String, Rect)], name: &str| {
        leaves
            .iter()
            .find(|(n, _)| n == name)
            .map_or(none, |(_, r)| *r)
    };
    let prompt_area = rect(&plan.leaves, "prompt");
    if layout.contains("message") {
        message = rect(&plan.leaves, "message");
    }

    // Docked panels go beside the chat on screen.
    let chat_area = rect(&plan.leaves, "chat");
    let chat_top = chat_area.y;
    let chat_area = app.allocate_panels(chat_area, false, &layout, &mut panels);
    for (name, r) in &mut plan.leaves {
        if name == "chat" {
            *r = chat_area;
        }
        if let Some(id) = name.strip_prefix("panel:") {
            if app.panel(id).is_some_and(|p| !p.hidden)
                && r.width > 0
                && r.height > 0
                && !panels.iter().any(|p| p.id == id)
            {
                panels.push(crate::panel::PanelPlacement {
                    id: id.to_owned(),
                    area: *r,
                    separator: None,
                });
            } else {
                r.width = 0;
                r.height = 0;
            }
        }
    }
    // Publish final geometry once, before painting or input can observe it.
    app.leaves = std::mem::take(&mut plan.leaves);
    app.seps = (plan.seps.iter())
        .map(|(x, area, _, path)| (Rect::new(*x, area.y, 1, area.height), path.clone()))
        .collect();
    app.publish_panels(&panels);
    app.placed = HashMap::from([
        (CHAT_WIN, Placed { area: chat_area }),
        (PROMPT_WIN, Placed { area: prompt_area }),
    ]);
    app.chat_expiry = None;
    // Every chat's own leaves, each once. Lua's chat queries default to the
    // chat a leaf is drawn for.
    let mut drawn = Vec::new();
    let mut cursor = None;
    for i in 0..app.leaves.len() {
        let (name, r) = app.leaves[i].clone();
        let Some((kind, buf)) = scoped(app, &name).filter(|s| !drawn.contains(s)) else {
            continue;
        };
        drawn.push((kind, buf));
        app.rendering = Some(buf);
        match kind {
            "chat" => draw_chat(frame, app, r, buf),
            "prompt" => cursor = cursor.or(draw_prompt(frame, app, r, buf)),
            _ => draw_line(frame, app, r, buf, kind),
        }
        crate::stats::mark(kind);
        app.rendering = None;
    }
    // A hidden chat keeps its data but not its rendered lines.
    for (b, c) in app.chats.iter_mut().enumerate() {
        if !drawn.contains(&("chat", b)) {
            c.redraw(None);
        }
    }
    crate::stats::mark("leaves");
    app.paint_panels(frame, &panels);
    crate::stats::mark("panels");
    for (name, r) in std::mem::take(&mut plan.regions) {
        // Measurement callbacks see available space; painting sees final space.
        if let Some((_, lines)) = app.region_sized(&name, r.width, r.height, true, true) {
            frame.render_widget(Paragraph::new(lines), r);
        }
    }
    for (x, area, sep, _) in std::mem::take(&mut plan.seps) {
        let style = app.theme.hl("WinSeparator");
        for y in area.y..area.bottom() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_symbol(&sep).set_style(style);
            }
        }
    }
    crate::stats::mark("regions");
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
                y: chat_top,
                height: prompt_top.saturating_sub(chat_top),
                ..prompt_area
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
    // Callbacks can hide/close panels after geometry was published. Do not
    // leave invisible panels in the last-frame geometry or mouse hit map.
    for placement in &panels {
        if app.panel(&placement.id).is_some_and(|p| !p.hidden) {
            continue;
        }
        frame.render_widget(Clear, placement.area);
        frame.render_widget(
            Block::default().style(app.theme.hl("Normal")),
            placement.area,
        );
        if let Some(p) = app.panel_mut(&placement.id) {
            p.area = None;
            p.view = 0;
        }
        for (name, rect) in &mut app.leaves {
            if name.strip_prefix("panel:") == Some(placement.id.as_str()) {
                rect.width = 0;
                rect.height = 0;
            }
        }
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
    crate::stats::mark("rest");
}

/// Where the layout put things, in the TUI's rects.
#[derive(Default)]
struct Plan {
    leaves: Vec<(String, Rect)>,
    regions: Vec<(String, Rect)>,
    seps: Vec<(u16, Rect, String, Vec<usize>)>,
}

/// A chat's own leaf and the chat it shows: bare for the chat on screen.
fn scoped(app: &App, name: &str) -> Option<(&'static str, BufferId)> {
    let k = BUILTIN[..4].iter().copied().find(|b| *b == kind(name))?;
    match name.split_once(':') {
        Some((_, id)) => Some((k, app.chat_by_session(id)?)),
        None => Some((k, app.current)),
    }
}

fn to_cells(r: Rect) -> layout::Rect {
    layout::Rect::new(r.x, r.y, r.width, r.height)
}

fn from_cells(r: layout::Rect) -> Rect {
    Rect::new(r.x, r.y, r.width, r.height)
}

/// Lay `node` out in `area` with the shared solver, measuring with `app`.
fn place(app: &mut App, node: &LayoutNode, area: Rect) -> Plan {
    let mut plan = layout::Plan::default();
    layout::place(
        &mut Measurer(app),
        node,
        to_cells(area),
        &mut plan,
        &mut Vec::new(),
    );
    let rects = |v: Vec<(String, layout::Rect)>| {
        v.into_iter()
            .map(|(n, r)| (n, from_cells(r)))
            .collect::<Vec<_>>()
    };
    Plan {
        leaves: rects(plan.leaves),
        regions: rects(plan.regions),
        seps: (plan.seps.into_iter())
            .map(|(x, r, sep, path)| (x, from_cells(r), sep, path))
            .collect(),
    }
}

/// What the layout asks while it sizes leaves.
struct Measurer<'a>(&'a mut App);

impl layout::Measure for Measurer<'_> {
    fn panel_hidden(&self, id: &str) -> bool {
        self.0.panel(id).is_none_or(|p| p.hidden)
    }

    fn prompt_height(&mut self, name: &str, width: u16) -> u16 {
        let app = &mut *self.0;
        let buf = scoped(app, name).map_or(app.current, |s| s.1);
        let text = app.prompt_of(buf);
        let ctx = app.prompt_ctx(width, buf, &text);
        let spec = app.prompt_spec(&ctx);
        bone_ui::prompt::natural_height(&spec, &text, width, app.options.prompt_max_height)
    }

    fn line_defined(&self, name: &str) -> bool {
        self.0.ui_defined(name)
    }

    fn message_rows(&self, area: layout::Rect) -> u16 {
        message_rows(self.0, from_cells(area))
    }

    fn panel_size(&mut self, id: &str, width: u16, height: u16, rows: bool) -> u16 {
        self.0.measure_panel(id, width, height, rows)
    }

    fn region_size(&mut self, name: &str, width: u16, height: u16, rows: bool) -> Option<u16> {
        self.0
            .region_sized(name, width, height, rows, false)
            .map(|(size, _)| size)
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

/// Draw chat `buf` in `area`: the chat on screen scrolls with `CHAT_WIN`,
/// others with their own view (`App::chat_views`).
fn draw_chat(frame: &mut Frame<'_>, app: &mut App, area: Rect, buf: BufferId) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let height = area.height as usize;
    app.render_chat(buf, area.width as usize);
    crate::stats::mark("render_chat");
    let chat = &app.chats[buf];
    let total = chat.row_count();
    let win = if buf == app.current {
        app.windows.get_mut(&CHAT_WIN).unwrap()
    } else {
        app.chat_views.entry(buf).or_default()
    };
    let max_top = total.saturating_sub(height);
    if win.follow || win.top >= max_top {
        win.top = max_top;
        // A following-dependent natural size may change on the next frame.
        app.redraw |= !win.follow;
        win.follow = true;
    }
    let lines: Vec<Line> = chat.rows().skip(win.top).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(lines), area);
    if total == 0
        && let Some((_, lines)) =
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

/// Draw chat `buf`'s prompt box: background, border and edge lines from
/// `bone.ui.prompt`, then the text (wrapped, scrolled and selected here, so
/// the cursor lands exactly). Returns where the cursor goes, for the chat
/// on screen.
fn draw_prompt(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    buf: BufferId,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let text = app.prompt_of(buf);
    let mut ctx = app.prompt_ctx(area.width, buf, &text);
    let spec = &app.prompt_spec(&ctx);
    let insets = Insets::of(spec, area.width);
    let inner = Rect {
        x: area.x + insets.left.min(area.width),
        y: area.y + insets.top.min(area.height),
        width: area.width.saturating_sub(insets.left + insets.right),
        height: area.height.saturating_sub(insets.top + insets.bottom),
    };
    let gutter = insets.gutter;
    let w = (inner.width as usize).saturating_sub(gutter).max(1);
    let (rows, (crow, ccol)) = prompt_layout(&text, w);
    let height = inner.height as usize;
    let live = buf == app.current;
    let mut draft = Window::default();
    let win = match live {
        true => app.windows.get_mut(&PROMPT_WIN).unwrap(),
        false => &mut draft,
    };
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

    let marks = if text.is_empty() || !live {
        Default::default()
    } else {
        app.prompt_highlight(&ctx)
    };
    let theme = &app.theme;
    let cursor_line = text.cursor();
    let at_line_end = text
        .lines()
        .get(cursor_line.0)
        .is_some_and(|l| cursor_line.1 >= l.chars().count());
    let selection = text.selection();
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

    let lines: Vec<Line> = if text.is_empty() {
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
    live.then_some(Position {
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

/// Chat `buf`'s statusline or divider row.
fn draw_line(frame: &mut Frame<'_>, app: &mut App, area: Rect, buf: BufferId, kind: &str) {
    if area.height == 0 {
        return;
    }
    let hl = if kind == "divider" {
        "WinSeparator"
    } else {
        "StatusLine"
    };
    let ctx = app.statusline_ctx(area.width, buf);
    let items = app.ui_items(kind, ctx, hl).unwrap_or_default();
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
    // Skipped/zero-sized windows must not keep bounds from an earlier frame.
    for p in &mut app.popups {
        p.rect = None;
    }
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
        // A render callback may have closed the window.
        let Some(p) = app.popups.iter_mut().find(|p| p.id == id) else {
            continue;
        };
        p.rect = Some(rect);
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
