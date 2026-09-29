//! The transcript, rendered exactly like the TUI: each row becomes a shared
//! [`bone_render::Message`] and is laid out by `bone_render::messages` into
//! styled terminal lines at the pane's width in character cells. Lines are
//! cached per row and invalidated by width, theme, content, or expansion
//! changes; only lines inside the viewport are painted.
use std::collections::HashMap;

use bone_protocol::{ChatRole, ToolCall, ToolDisplayConfig, ToolResult};
use bone_render::{Message, messages, theme::Theme, tool_display, transcript};
use eframe::egui::{self, Ui};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::grid::{self, TermRow};
use crate::state::{ToolCard, ToolState};

pub(crate) struct Cache {
    /// Width in character cells the cached lines were laid out at.
    cols: u16,
    /// Per-row terminal rows, wrapped exactly as the TUI draws them; `None`
    /// means not laid out yet.
    lines: Vec<Option<Vec<TermRow>>>,
    /// Per-row expansion (the TUI's expanded tool output), toggled by clicking.
    expanded: Vec<bool>,
    /// Ctrl+O: expand every row (a clicked row flips against it).
    expand_all: bool,
    /// Mouse selection over the flattened lines, for copying.
    selection: Option<Selection>,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            cols: 0,
            lines: Vec::new(),
            expanded: Vec::new(),
            expand_all: false,
            selection: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Ctrl+O: expand or collapse every tool row, like the TUI.
    pub(crate) fn toggle_all(&mut self) {
        self.expand_all = !self.expand_all;
        self.invalidate();
    }

    /// Drop every cached layout (theme or tool display changed).
    pub(crate) fn invalidate(&mut self) {
        self.lines.iter_mut().for_each(|lines| *lines = None);
    }

    /// Match the row count and drop layouts of rows whose content changed.
    pub(crate) fn sync(&mut self, rows_len: usize, changed: &[usize]) {
        self.lines.resize(rows_len, None);
        self.expanded.resize(rows_len, false);
        for &i in changed {
            if let Some(lines) = self.lines.get_mut(i) {
                *lines = None;
            }
            // A row's neighbour decides its leading blank line.
            if let Some(lines) = self.lines.get_mut(i + 1) {
                *lines = None;
            }
        }
    }

    /// Older rows were prepended above the loaded ones.
    pub(crate) fn prepend_rows(&mut self, added: usize, rows_len: usize) {
        let mut lines = vec![None; added];
        lines.append(&mut self.lines);
        self.lines = lines;
        let mut expanded = vec![false; added];
        expanded.append(&mut self.expanded);
        self.expanded = expanded;
        self.sync(rows_len, &[added.saturating_sub(1)]);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        stick_to_bottom: bool,
        rows: &[(String, String)],
        cards: &[Option<ToolCard>],
        displays: &HashMap<String, ToolDisplayConfig>,
        theme: &Theme,
    ) -> egui::scroll_area::ScrollAreaOutput<()> {
        self.sync(rows.len(), &[]);
        let metrics = grid::metrics(ui);
        let (row_height, cell) = (metrics.row_height, metrics.cell);
        let scrollbar = ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_inner_margin;
        let cols = (((ui.available_width() - scrollbar) / cell).floor() as u16).max(20);
        if cols != self.cols {
            self.cols = cols;
            self.invalidate();
            self.selection = None;
        }
        for i in 0..rows.len() {
            self.layout(i, rows, cards, displays, theme);
        }
        let tops: Vec<f32> = self
            .lines
            .iter()
            .scan(0.0, |y, lines| {
                let top = *y;
                *y += lines.as_ref().map_or(0, Vec::len) as f32 * row_height;
                Some(top)
            })
            .collect();
        let total = self.height(row_height);
        let default_fg = ui.visuals().text_color();
        let mut toggled = None;
        let mut selection = self.selection;
        let cols = self.cols;
        let mut output = egui::ScrollArea::vertical()
            .id_salt("transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(stick_to_bottom)
            .show_viewport(ui, |ui, viewport| {
                ui.set_height(total);
                ui.set_width(ui.available_width());
                let origin = ui.max_rect().min;
                let total_lines = (total / row_height).round() as usize;
                let locate = |pos: egui::Pos2| -> (usize, u16) {
                    let line = (((pos.y - origin.y) / row_height).floor().max(0.0) as usize)
                        .min(total_lines.saturating_sub(1));
                    let col = (((pos.x - origin.x) / cell).round().max(0.0) as u16).min(cols);
                    (line, col)
                };
                let row_at = |line: usize| -> Option<usize> {
                    let y = line as f32 * row_height + row_height / 2.0;
                    tops.partition_point(|top| *top <= y).checked_sub(1)
                };
                // First flattened line of row `i`.
                let row_line = |i: usize| (tops[i] / row_height).round() as usize;
                let tool_row_at = |line: usize| -> Option<usize> {
                    let i = row_at(line)?;
                    (rows[i].0.starts_with("tool:") && !self.lines[i].as_deref()?.is_empty())
                        .then_some(i)
                };
                // On a touch screen the viewport only senses clicks: a click
                // widget that senses drags would win the hit test against the
                // scroll area below it, turning every touch drag into a
                // selection. Click-only keeps dragging where it belongs.
                let touch = ui.input(|input| input.has_touch_screen());
                let hit = if touch {
                    egui::Rect::from_min_size(origin, ui.max_rect().size())
                } else {
                    egui::Rect::from_min_size(origin, egui::vec2(ui.max_rect().width(), total))
                };
                let response = ui.interact(
                    hit,
                    ui.id().with("transcript-select"),
                    if touch {
                        egui::Sense::click()
                    } else {
                        egui::Sense::click_and_drag()
                    },
                );
                if let Some(pos) = response.hover_pos() {
                    ui.ctx()
                        .set_cursor_icon(if tool_row_at(locate(pos).0).is_some() {
                            egui::CursorIcon::PointingHand
                        } else {
                            egui::CursorIcon::Text
                        });
                }
                let long_touched = response.long_touched();
                if long_touched {
                    // Long press on a touch screen: select the word under the
                    // finger, and offer Copy in the context menu below.
                    selection = response.interact_pointer_pos().and_then(|pos| {
                        let at = locate(pos);
                        let i = row_at(at.0)?;
                        let offset = at.0.checked_sub(row_line(i))?;
                        let line = self.lines[i].as_deref()?.get(offset)?;
                        let (from, to) = word_span(line, at.1);
                        (to > from).then_some(((at.0, from), (at.0, to)))
                    });
                } else if let Some(pos) = response.interact_pointer_pos() {
                    if response.drag_started() {
                        selection = Some((locate(pos), locate(pos)));
                    } else if response.dragged()
                        && let Some((anchor, _)) = selection
                    {
                        selection = Some((anchor, locate(pos)));
                    }
                    if response.clicked() {
                        selection = None;
                        toggled = tool_row_at(locate(pos).0);
                    }
                }
                let sel = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
                let mut copy = false;
                if touch {
                    response.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            copy = true;
                            ui.close();
                        }
                    });
                }
                let shortcut =
                    ui.input(|input| input.events.iter().any(|e| matches!(e, egui::Event::Copy)));
                if let Some((start, end)) = sel
                    && (copy || shortcut)
                {
                    let flat: Vec<&TermRow> = self
                        .lines
                        .iter()
                        .flat_map(|lines| lines.as_deref().unwrap_or_default().iter())
                        .collect();
                    ui.ctx().copy_text(selected_text(&flat, start, end));
                    ui.input_mut(|input| input.events.retain(|e| !matches!(e, egui::Event::Copy)));
                }
                let first = tops.partition_point(|top| *top + row_height <= viewport.min.y);
                for (i, &top) in tops.iter().enumerate().skip(first.saturating_sub(1)) {
                    if top > viewport.max.y {
                        break;
                    }
                    let lines = self.lines[i].as_deref().unwrap_or_default();
                    for (n, line) in lines.iter().enumerate() {
                        let y = origin.y + top + n as f32 * row_height;
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(origin.x, y),
                            egui::vec2(ui.max_rect().width(), row_height),
                        );
                        let g = (top / row_height).round() as usize + n;
                        if let Some((start, end)) = sel
                            && let Some((from, to)) = selected_span(g, start, end, cols)
                        {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(origin.x + from as f32 * cell, y),
                                    egui::pos2(origin.x + to as f32 * cell, y + row_height),
                                ),
                                0.0,
                                ui.visuals().selection.bg_fill,
                            );
                        }
                        grid::paint_row(ui, rect, &metrics, line, default_fg);
                    }
                }
            });
        self.selection = selection;
        if let Some(i) = toggled {
            self.expanded[i] = !self.expanded[i];
            self.lines[i] = None;
            // Lay the row out now and fix the scroll offset for the new height
            // before the next frame paints. egui only clamps (or re-sticks) the
            // offset after painting the content, so leaving it would draw one
            // frame at the stale offset: blank after collapsing a long output,
            // or shifted after expanding at the bottom. No repaint follows that
            // correction, so the wrong frame stayed up until the pointer moved.
            self.layout(i, rows, cards, displays, theme);
            let total = self.height(row_height);
            let max_offset = (total - output.inner_rect.height()).max(0.0);
            output.state.offset.y = if stick_to_bottom {
                max_offset
            } else {
                output.state.offset.y.min(max_offset)
            };
            output.content_size.y = total;
            output.state.store(ui.ctx(), output.id);
            ui.ctx().request_repaint();
        }
        output
    }

    /// Lay out row `i` into terminal rows unless it is already cached.
    fn layout(
        &mut self,
        i: usize,
        rows: &[(String, String)],
        cards: &[Option<ToolCard>],
        displays: &HashMap<String, ToolDisplayConfig>,
        theme: &Theme,
    ) {
        if self.lines[i].is_some() {
            return;
        }
        let prev = i.checked_sub(1).map(|p| row_role(&rows[p].0));
        let lines = row_message(&rows[i], cards.get(i).and_then(Option::as_ref), displays)
            .map(|message| {
                let lines = messages::msg_to_lines(
                    &[message],
                    theme,
                    prev,
                    self.cols,
                    self.expanded[i] != self.expand_all,
                );
                terminal_rows(&lines, self.cols, theme)
            })
            .unwrap_or_default();
        self.lines[i] = Some(lines);
    }

    /// Total content height of the laid-out rows.
    fn height(&self, row_height: f32) -> f32 {
        self.lines
            .iter()
            .map(|lines| lines.as_ref().map_or(0, Vec::len))
            .sum::<usize>() as f32
            * row_height
    }
}

/// A selection over flattened transcript lines: (line, cell column) anchor
/// and head, in either order.
type Selection = ((usize, u16), (usize, u16));

/// Cell range of line `g` inside the ordered selection `(start, end)`.
fn selected_span(
    g: usize,
    start: (usize, u16),
    end: (usize, u16),
    cols: u16,
) -> Option<(u16, u16)> {
    if g < start.0 || g > end.0 {
        return None;
    }
    let from = if g == start.0 { start.1 } else { 0 };
    let to = if g == end.0 { end.1 } else { cols };
    (to > from).then_some((from, to))
}

/// Plain text of the cells `from..to` of one terminal row.
fn row_text(line: &TermRow, from: u16, to: u16) -> String {
    let mut text = String::new();
    for run in line {
        let mut col = run.col;
        for ch in run.text.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(1)
                .max(1) as u16;
            if col >= from && col < to {
                text.push(ch);
            }
            col += width;
        }
    }
    text.trim_end().to_owned()
}

/// Character range of the word at cell `col` of one terminal row. `(c, c)`
/// when that cell is not part of a word, so callers can ignore empty spans.
fn word_span(line: &TermRow, col: u16) -> (u16, u16) {
    let mut cells: Vec<(u16, char)> = Vec::new();
    for run in line {
        let mut cell = run.col;
        for ch in run.text.chars() {
            cells.push((cell, ch));
            cell = cell.saturating_add(
                unicode_width::UnicodeWidthChar::width(ch)
                    .unwrap_or(1)
                    .max(1) as u16,
            );
        }
    }
    let Some(at) = cells.iter().rposition(|&(cell, _)| cell <= col) else {
        return (col, col);
    };
    let word = |ch: char| ch.is_alphanumeric() || ch == '_';
    if !word(cells[at].1) {
        return (cells[at].0, cells[at].0);
    }
    let start = cells[..at]
        .iter()
        .rposition(|&(_, ch)| !word(ch))
        .map_or(0, |i| i + 1);
    let end = cells[at + 1..]
        .iter()
        .position(|&(_, ch)| !word(ch))
        .map_or(cells.len() - 1, |i| at + i);
    let last = cells[end];
    (
        cells[start].0,
        last.0.saturating_add(
            unicode_width::UnicodeWidthChar::width(last.1)
                .unwrap_or(1)
                .max(1) as u16,
        ),
    )
}

/// Plain text of an ordered selection over flattened lines.
fn selected_text(flat: &[&TermRow], start: (usize, u16), end: (usize, u16)) -> String {
    (start.0..=end.0)
        .filter_map(|g| {
            let line = flat.get(g)?;
            let (from, to) = selected_span(g, start, end, u16::MAX).unwrap_or((0, 0));
            Some(row_text(line, from, to))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The TUI role a desktop row renders as.
fn row_role(role: &str) -> ChatRole {
    match role {
        "user" | "attachments" => ChatRole::User,
        "assistant" => ChatRole::Assistant,
        role if role.starts_with("tool:") => ChatRole::Tool,
        _ => ChatRole::System,
    }
}

/// Convert a desktop row into the display message the TUI would show, using
/// the same shared builders. `None` for rows the TUI does not display.
fn row_message(
    (role, text): &(String, String),
    card: Option<&ToolCard>,
    displays: &HashMap<String, ToolDisplayConfig>,
) -> Option<Message> {
    match role.as_str() {
        "user" => Some(Message::user(text.clone())),
        "assistant" => transcript::assistant_display_message(text),
        "attachments" => None,
        role if role.starts_with("tool:") => {
            let card = card?;
            let is_error = card.state == ToolState::Error;
            if let Some(diff) = transcript::edit_diff_message(&card.name, is_error, text) {
                return Some(diff);
            }
            let display = displays.get(&card.name);
            // Like the TUI, a running call shows only when its tool is eager;
            // running shells appear in the strip above the input instead.
            if card.state == ToolState::Running && !display.and_then(|d| d.eager).unwrap_or(false) {
                return None;
            }
            let arguments = card
                .args
                .as_deref()
                .and_then(|args| serde_json::from_str(args).ok());
            let Some(arguments) = arguments else {
                // A local inline shell row keeps its command line, not JSON.
                if card.name == "shell" {
                    let command = card.args.clone().unwrap_or_default();
                    return Some(tool_display::shell_row(&command, text.clone(), is_error));
                }
                return transcript::orphaned_tool_result_row(card.name.clone(), is_error);
            };
            let call = ToolCall {
                id: String::new(),
                name: card.name.clone(),
                arguments,
            };
            let result = ToolResult {
                name: card.name.clone(),
                content: if card.state == ToolState::Running {
                    String::new()
                } else {
                    text.clone()
                },
                is_error,
                ..Default::default()
            };
            Some(tool_display::build_tool_row(
                &call,
                &result,
                displays.get(&card.name),
            ))
        }
        _ => Some(Message::system(text.clone())),
    }
}

/// Lay logical lines out into terminal rows the way the TUI draws scrollback:
/// each line word-wrapped to `cols` cells, user lines on a full-width band.
fn terminal_rows(lines: &[ratatui::text::Line<'static>], cols: u16, theme: &Theme) -> Vec<TermRow> {
    let height = messages::logical_lines_row_count(lines, cols);
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, height));
    messages::render_scrollback_lines_with_bg(lines, &mut buffer, Some(theme.user_msg_bg));
    grid::buffer_rows(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, args: serde_json::Value, state: ToolState) -> Option<ToolCard> {
        Some(ToolCard {
            name: name.into(),
            state,
            args: Some(args.to_string()),
            started: None,
        })
    }

    #[test]
    fn rows_map_to_the_tui_display_messages() {
        let displays = HashMap::new();
        let user = row_message(&("user".into(), "hi".into()), None, &displays).unwrap();
        assert_eq!(user.role, ChatRole::User);
        assert!(
            row_message(
                &("assistant".into(), "<timing>x</timing>".into()),
                None,
                &displays
            )
            .is_none()
        );
        let read = row_message(
            &(
                "tool: read_file".into(),
                "File: a.rs\nRange: 1-2\n1 | a\n2 | b".into(),
            ),
            card(
                "read_file",
                serde_json::json!({"path": "a.rs"}),
                ToolState::Done,
            )
            .as_ref(),
            &displays,
        )
        .unwrap();
        assert!(read.tool.unwrap().label.starts_with("read_file a.rs"));
        let shell = row_message(
            &("tool: shell".into(), "exit code: 0".into()),
            Some(&ToolCard {
                name: "shell".into(),
                state: ToolState::Done,
                args: Some("ls -la".into()),
                started: None,
            }),
            &displays,
        )
        .unwrap();
        assert!(shell.tool.unwrap().is_shell);
    }

    #[test]
    fn show_lays_out_rows_as_terminal_lines_and_toggles_tool_rows() {
        let rows = vec![
            ("user".to_string(), "hello".to_string()),
            (
                "assistant".to_string(),
                "# Title\n\nSome **bold** text.".to_string(),
            ),
        ];
        let cards = vec![None, None];
        let theme = Theme::default();
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            cache.show(ui, true, &rows, &cards, &HashMap::new(), &theme);
        });
        output.textures_delta.clear();
        assert!(cache.cols >= 20);
        let lines = cache.lines[1].as_ref().unwrap();
        let text: String = lines
            .iter()
            .flat_map(|row| row.iter().map(|run| run.text.clone()))
            .collect();
        assert!(text.contains("Title") && text.contains("bold"), "{text}");
    }

    /// Clicking a tool row must leave the scroll offset valid for the new
    /// height before the next frame paints; otherwise that frame is drawn at
    /// the stale offset (a blank or shifted chat).
    #[test]
    fn toggling_a_tool_row_fixes_the_scroll_offset_before_the_next_frame() {
        let output: String = (1..=200).map(|n| format!("{n}\n")).collect();
        let rows = vec![
            ("user".to_string(), "count".to_string()),
            ("tool: shell".to_string(), output),
        ];
        let cards = vec![
            None,
            Some(ToolCard {
                name: "shell".into(),
                state: ToolState::Done,
                args: Some("seq 200".into()),
                started: None,
            }),
        ];
        let theme = Theme::default();
        let ctx = egui::Context::default();
        let mut cache = Cache::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 240.0));
        let frame = |events: Vec<egui::Event>, cache: &mut Cache| {
            let mut result = None;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| result = Some(cache.show(ui, true, &rows, &cards, &HashMap::new(), &theme)),
            );
            out.textures_delta.clear();
            result.unwrap()
        };
        let click = |pos| {
            let press = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), press(true), press(false)]
        };
        let bottom = |output: &egui::scroll_area::ScrollAreaOutput<()>| {
            (output.content_size.y - output.inner_rect.height()).max(0.0)
        };

        // The tool row's first line, in screen coordinates.
        let tool_row = |output: &egui::scroll_area::ScrollAreaOutput<()>, cache: &Cache| {
            let line_count: usize = cache.lines.iter().flatten().map(Vec::len).sum();
            let row_height = output.content_size.y / line_count as f32;
            let top = cache.lines[0].as_ref().map_or(0, Vec::len) as f32 * row_height;
            egui::pos2(
                output.inner_rect.left() + 10.0,
                output.inner_rect.top() + top - output.state.offset.y + row_height / 2.0,
            )
        };

        frame(Vec::new(), &mut cache);
        let first = frame(Vec::new(), &mut cache);
        let collapsed = first.content_size.y;
        let tap = tool_row(&first, &cache);
        frame(vec![egui::Event::PointerMoved(tap)], &mut cache);

        let expanded = frame(click(tap), &mut cache);
        assert!(cache.expanded[1]);
        let stored = egui::scroll_area::State::load(&ctx, expanded.id).unwrap();
        assert!(
            expanded.content_size.y > collapsed,
            "{} {}",
            expanded.content_size.y,
            collapsed
        );
        assert_eq!(stored.offset.y, bottom(&expanded));

        // Settle at the bottom of the expanded output, where a collapse would
        // otherwise leave the viewport past the end of the content.
        let settled = frame(Vec::new(), &mut cache);
        assert_eq!(settled.state.offset.y, bottom(&settled));
        let tap = tool_row(&settled, &cache);
        let tap = egui::pos2(tap.x, tap.y.max(settled.inner_rect.top() + 2.0));
        frame(vec![egui::Event::PointerMoved(tap)], &mut cache);
        let collapsed = frame(click(tap), &mut cache);
        assert!(!cache.expanded[1]);
        let stored = egui::scroll_area::State::load(&ctx, collapsed.id).unwrap();
        assert!(stored.offset.y <= bottom(&collapsed), "{}", stored.offset.y);
    }
    /// Frame-by-frame harness over one transcript, for pointer and touch tests.
    struct Harness {
        ctx: egui::Context,
        cache: Cache,
        rows: Vec<(String, String)>,
        cards: Vec<Option<ToolCard>>,
        screen: egui::Rect,
        cell: f32,
        row_height: f32,
    }

    impl Harness {
        fn new(rows: Vec<(String, String)>, cards: Vec<Option<ToolCard>>) -> Self {
            Self {
                ctx: egui::Context::default(),
                cache: Cache::new(),
                rows,
                cards,
                screen: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 240.0)),
                cell: 0.0,
                row_height: 0.0,
            }
        }

        fn frame(
            &mut self,
            time: f64,
            events: Vec<egui::Event>,
        ) -> egui::scroll_area::ScrollAreaOutput<()> {
            let ctx = self.ctx.clone();
            let Self {
                cache,
                rows,
                cards,
                screen,
                cell,
                row_height,
                ..
            } = self;
            let theme = Theme::default();
            let mut metrics = None;
            let mut result = None;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(*screen),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    metrics = Some(grid::metrics(ui));
                    result = Some(cache.show(ui, false, rows, cards, &HashMap::new(), &theme));
                },
            );
            out.textures_delta.clear();
            let metrics = metrics.expect("one frame");
            *cell = metrics.cell;
            *row_height = metrics.row_height;
            result.expect("one frame")
        }

        /// Centre of the cell `(line, col)` of the flattened transcript.
        fn cell_pos(
            &self,
            output: &egui::scroll_area::ScrollAreaOutput<()>,
            line: usize,
            col: u16,
        ) -> egui::Pos2 {
            egui::pos2(
                output.inner_rect.left() + (col as f32 + 0.5) * self.cell,
                output.inner_rect.top() - output.state.offset.y
                    + (line as f32 + 0.5) * self.row_height,
            )
        }

        /// Plain text of the selected span.
        fn selected(&self) -> String {
            let sel = self.cache.selection.expect("a selection");
            let (start, end) = if sel.0 <= sel.1 { sel } else { (sel.1, sel.0) };
            let flat: Vec<&TermRow> = self
                .cache
                .lines
                .iter()
                .flat_map(|lines| lines.as_deref().unwrap_or_default().iter())
                .collect();
            selected_text(&flat, start, end)
        }
    }

    /// What egui-winit feeds egui for one finger: the touch event plus the
    /// pointer events it synthesises from it.
    fn touch_events(phase: egui::TouchPhase, pos: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut events = vec![
            egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: None,
            },
            egui::Event::PointerMoved(pos),
        ];
        match phase {
            egui::TouchPhase::Start => events.push(button(true)),
            egui::TouchPhase::End => {
                events.push(button(false));
                events.push(egui::Event::PointerGone);
            }
            _ => {}
        }
        events
    }

    /// First cell of `needle` in the flattened transcript lines.
    fn cell_of(cache: &Cache, needle: &str) -> (usize, u16) {
        let mut line = 0;
        for lines in &cache.lines {
            for row in lines.as_deref().unwrap_or_default() {
                for run in row {
                    if let Some(byte) = run.text.find(needle) {
                        let width: u16 = run.text[..byte]
                            .chars()
                            .map(|ch| {
                                unicode_width::UnicodeWidthChar::width(ch)
                                    .unwrap_or(1)
                                    .max(1) as u16
                            })
                            .sum();
                        return (line, run.col + width);
                    }
                }
                line += 1;
            }
        }
        panic!("{needle:?} is not in the transcript");
    }

    fn run_row(col: u16, text: &str) -> TermRow {
        vec![grid::Run {
            col,
            cells: text.chars().count() as u16,
            text: text.to_owned(),
            style: ratatui::style::Style::default(),
        }]
    }

    #[test]
    fn word_span_covers_the_word_under_the_cell() {
        let line = run_row(0, "hello, world_x");
        assert_eq!(word_span(&line, 0), (0, 5));
        assert_eq!(word_span(&line, 4), (0, 5));
        assert_eq!(word_span(&line, 5), (5, 5)); // a comma is not a word
        assert_eq!(word_span(&line, 7), (7, 14));
        assert_eq!(word_span(&line, 13), (7, 14));
        // Past the end of the line: the last word is still what was pressed.
        assert_eq!(word_span(&line, 40), (7, 14));
        // A wide glyph is its own pair of cells.
        assert_eq!(word_span(&run_row(0, "\u{65e5}\u{672c} go"), 3), (0, 4));
    }

    /// A finger drag scrolls the transcript; it must never start a selection.
    #[test]
    fn a_touch_drag_scrolls_instead_of_selecting() {
        let text: String = (1..=200).map(|n| format!("line {n}\n")).collect();
        let mut h = Harness::new(vec![("assistant".to_string(), text)], vec![None]);
        // The first touch makes egui report a touch screen from then on.
        h.frame(
            0.0,
            touch_events(egui::TouchPhase::Start, egui::pos2(10.0, 10.0)),
        );
        let warm = h.frame(
            0.05,
            touch_events(egui::TouchPhase::End, egui::pos2(10.0, 10.0)),
        );
        let at = h.cell_pos(&warm, 5, 3);

        h.frame(0.1, touch_events(egui::TouchPhase::Start, at));
        // 60 px towards the top of the screen: past egui's click slop.
        let dragged = h.frame(
            0.15,
            touch_events(egui::TouchPhase::Move, at - egui::vec2(0.0, 60.0)),
        );
        assert!(
            dragged.state.offset.y > 1.0,
            "the drag scrolled to {}",
            dragged.state.offset.y
        );
        assert!(h.cache.selection.is_none(), "a drag must not select");
    }

    /// A long press selects the word under the finger, for the Copy menu.
    #[test]
    fn a_long_press_selects_the_word_under_the_finger() {
        let mut h = Harness::new(
            vec![("user".to_string(), "hello world".to_string())],
            vec![None],
        );
        h.frame(
            0.0,
            touch_events(egui::TouchPhase::Start, egui::pos2(10.0, 10.0)),
        );
        let warm = h.frame(
            0.05,
            touch_events(egui::TouchPhase::End, egui::pos2(10.0, 10.0)),
        );
        let (line, col) = cell_of(&h.cache, "hello");
        let at = h.cell_pos(&warm, line, col);

        h.frame(0.1, touch_events(egui::TouchPhase::Start, at));
        // Hold still: egui calls it a long press after 0.8 s.
        for step in 1..=20 {
            h.frame(
                0.1 + 0.05 * f64::from(step),
                touch_events(egui::TouchPhase::Move, at),
            );
        }
        let sel = h.cache.selection.expect("a long press selects a word");
        let (start, end) = if sel.0 <= sel.1 { sel } else { (sel.1, sel.0) };
        assert_eq!(start, (line, col), "the word is selected in place");
        assert_eq!(end.0, line);
        assert_eq!(h.selected(), "hello");
    }

    /// Tapping a tool row still expands it on a touch screen.
    #[test]
    fn a_touch_tap_still_toggles_a_tool_row() {
        let output: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let mut h = Harness::new(
            vec![
                ("user".to_string(), "count".to_string()),
                ("tool: shell".to_string(), output),
            ],
            vec![None, card("shell", serde_json::json!({}), ToolState::Done)],
        );
        h.frame(
            0.0,
            touch_events(egui::TouchPhase::Start, egui::pos2(10.0, 10.0)),
        );
        let warm = h.frame(
            0.05,
            touch_events(egui::TouchPhase::End, egui::pos2(10.0, 10.0)),
        );
        // The tool row's first line.
        let line = h.cache.lines[0].as_ref().map_or(0, Vec::len);
        let at = h.cell_pos(&warm, line, 2);

        h.frame(0.1, touch_events(egui::TouchPhase::Start, at));
        h.frame(0.2, touch_events(egui::TouchPhase::End, at));
        assert!(h.cache.expanded[1], "a tap expands the tool row");
    }

    /// A mouse drag still selects text, and does not scroll.
    #[test]
    fn a_mouse_drag_still_selects_text() {
        let mut h = Harness::new(
            vec![("assistant".to_string(), "alpha beta".to_string())],
            vec![None],
        );
        h.frame(0.0, Vec::new());
        let warm = h.frame(0.05, Vec::new());
        let (line, col) = cell_of(&h.cache, "alpha");
        let press = |pressed| egui::Event::PointerButton {
            pos: h.cell_pos(&warm, line, col),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        h.frame(
            0.1,
            vec![
                egui::Event::PointerMoved(h.cell_pos(&warm, line, col)),
                press(true),
            ],
        );
        let out = h.frame(
            0.15,
            vec![egui::Event::PointerMoved(h.cell_pos(&warm, line, col + 5))],
        );
        h.frame(
            0.2,
            vec![egui::Event::PointerMoved(h.cell_pos(&warm, line, col + 9))],
        );
        assert!(!h.selected().trim().is_empty(), "a mouse drag selects text");
        assert_eq!(out.state.offset.y, 0.0, "a mouse drag must not scroll");
    }
}
