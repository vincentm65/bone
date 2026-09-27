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
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            cols: 0,
            lines: Vec::new(),
            expanded: Vec::new(),
            expand_all: false,
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
        }
        for i in 0..rows.len() {
            if self.lines[i].is_none() {
                let prev = i.checked_sub(1).map(|p| row_role(&rows[p].0));
                let lines = row_message(&rows[i], cards.get(i).and_then(Option::as_ref), displays)
                    .map(|message| {
                        let lines = messages::msg_to_lines(
                            &[message],
                            theme,
                            prev,
                            cols,
                            self.expanded[i] != self.expand_all,
                        );
                        terminal_rows(&lines, cols, theme)
                    })
                    .unwrap_or_default();
                self.lines[i] = Some(lines);
            }
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
        let total = self
            .lines
            .last()
            .zip(tops.last())
            .map_or(0.0, |(lines, top)| {
                top + lines.as_ref().map_or(0, Vec::len) as f32 * row_height
            });
        let default_fg = ui.visuals().text_color();
        let mut toggled = None;
        let output = egui::ScrollArea::vertical()
            .id_salt("transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(stick_to_bottom)
            .show_viewport(ui, |ui, viewport| {
                ui.set_height(total);
                ui.set_width(ui.available_width());
                let origin = ui.max_rect().min;
                let first = tops.partition_point(|top| *top + row_height <= viewport.min.y);
                for i in first.saturating_sub(1)..rows.len() {
                    let top = tops[i];
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
                        grid::paint_row(ui, rect, &metrics, line, default_fg);
                    }
                    if rows[i].0.starts_with("tool:") && !lines.is_empty() {
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(origin.x, origin.y + top),
                            egui::vec2(ui.max_rect().width(), lines.len() as f32 * row_height),
                        );
                        let response = ui
                            .interact(rect, ui.id().with(("tool-row", i)), egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            toggled = Some(i);
                        }
                    }
                }
            });
        if let Some(i) = toggled {
            self.expanded[i] = !self.expanded[i];
            self.lines[i] = None;
            ui.ctx().request_repaint();
        }
        output
    }
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
}
