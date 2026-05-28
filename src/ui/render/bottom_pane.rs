use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::wrap;
use super::{InputState, Prompt, SPINNER, StatusInfo};
use crate::tools::ApprovalMode;
use crate::ui::pane_page::PanePage;
use crate::ui::tool_display;

const COMMAND_PREVIEW_LINES: usize = 6;
const BLANK_CURSOR_CELL: &str = "\u{00a0}";

/// Maximum rows of pane page content visible at once.
pub const MAX_PANE_ROWS: usize = 8;

fn input_width(terminal_width: u16) -> usize {
    terminal_width.saturating_sub(1).max(1) as usize
}

fn shell_prompt_title(prompt: &Prompt) -> String {
    format!(
        "  {}",
        prompt.title.split(" — ").next().unwrap_or(&prompt.title)
    )
}

fn shell_command_preview_lines(command: &str, width: usize) -> Vec<String> {
    tool_display::format_shell_command(command)
        .into_iter()
        .flat_map(|line| wrap::wrap_text_with_prefix(&line, "  ", "  ", width))
        .collect()
}

fn separator_with_hint(width: u16, hint: Option<&str>) -> String {
    let width = width as usize;
    let Some(hint) = hint else {
        return "─".repeat(width);
    };
    let label = format!(" {hint} ");
    if label.len() >= width {
        return "─".repeat(width);
    }
    let remaining = width - label.len();
    let left = remaining.saturating_sub(2);
    format!("{}{label}──", "─".repeat(left.max(0)))
}

/// Split input buffer at cursor into (before, char-at-cursor, after).
fn cursor_split(input: &InputState) -> (String, char, String) {
    let chars: Vec<char> = input.buffer.chars().collect();
    let pos = input.cursor_pos.min(chars.len());
    let before: String = chars[..pos].iter().collect();
    let at_cursor = *chars.get(pos).unwrap_or(&' ');
    let after: String = chars[pos..].iter().skip(1).collect();
    (before, at_cursor, after)
}

fn push_input_text(text: &str, spans: &mut Vec<Span<'static>>, lines: &mut Vec<Line<'static>>) {
    for (index, part) in text.split('\n').enumerate() {
        if index > 0 {
            lines.push(Line::from(std::mem::take(spans)));
        }
        if !part.is_empty() {
            spans.push(Span::raw(part.to_string()));
        }
    }
}

/// Build actual logical lines so hard newlines in pasted input render as rows.
fn input_text(input: &InputState) -> Text<'static> {
    let (before, at_cursor, after) = cursor_split(input);
    let mut spans = vec![Span::raw("> ")];
    let mut lines = Vec::new();

    push_input_text(&before, &mut spans, &mut lines);
    if at_cursor == '\n' {
        spans.push(Span::styled(
            BLANK_CURSOR_CELL,
            Style::default().add_modifier(Modifier::REVERSED),
        ));
        lines.push(Line::from(std::mem::take(&mut spans)));
    } else {
        let cursor_cell = if at_cursor == ' ' {
            BLANK_CURSOR_CELL.to_string()
        } else {
            at_cursor.to_string()
        };
        spans.push(Span::styled(
            cursor_cell,
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }
    push_input_text(&after, &mut spans, &mut lines);
    lines.push(Line::from(spans));

    Text::from(lines)
}

fn rendered_input_rows(input: &InputState, terminal_width: u16) -> u16 {
    Paragraph::new(input_text(input))
        .wrap(Wrap { trim: false })
        .line_count(input_width(terminal_width) as u16)
        .max(1) as u16
}

/// Compute how many rows a page's visible content occupies.
fn page_visible_rows(page: &PanePage) -> usize {
    page.content
        .len()
        .saturating_sub(page.scroll)
        .min(MAX_PANE_ROWS)
}

/// Compute extra height needed for the page region (separators + content + tab indicator).
fn page_extra_height(pages: &[PanePage], active_page: usize) -> u16 {
    if pages.is_empty() {
        return 0;
    }
    let page = &pages[active_page.min(pages.len() - 1)];
    let content_rows = page_visible_rows(page) as u16;
    // page top sep + content + optional page bottom sep/tab indicator.
    // Single-page panes use the fixed bottom separator as the closing rule.
    let tab_indicator = if pages.len() > 1 { 1u16 } else { 0u16 };
    let page_bottom_sep = if pages.len() > 1 { 1u16 } else { 0u16 };
    1 + content_rows + page_bottom_sep + tab_indicator
}

impl super::Renderer {
    /// Draw the bottom pane into the fixed inline viewport.
    pub fn draw_bottom_pane(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        prompt: Option<&Prompt>,
        pages: &[PanePage],
        active_page: usize,
        pane_toggle_hint: Option<&str>,
    ) {
        self.draw_bottom_pane_with_tick(
            frame,
            input,
            status_info,
            self.spinner_tick,
            prompt,
            pages,
            active_page,
            pane_toggle_hint,
        );
    }

    /// Compute the desired viewport height for the current state.
    pub fn desired_height(
        input: &InputState,
        prompt: Option<&Prompt>,
        terminal_width: u16,
        pages: &[PanePage],
        active_page: usize,
    ) -> u16 {
        if let Some(p) = prompt {
            let options = p.options.len().min(p.visible_rows) as u16;
            let hint = u16::from(p.hint.is_some());
            let prompt_rows = if let Some(ref cmd) = p.full_command {
                let title = shell_prompt_title(p);
                let title_lines = wrap::wrap_text(&title, terminal_width as usize).len() as u16;
                let cmd_visual_lines =
                    shell_command_preview_lines(cmd, terminal_width as usize).len() as u16;
                if p.peek_mode {
                    // title + cmd_lines + hint + options
                    title_lines + cmd_visual_lines + hint + options
                } else {
                    // title + preview + hint + options
                    let preview = cmd_visual_lines.min(COMMAND_PREVIEW_LINES as u16);
                    title_lines + preview + hint + options
                }
            } else {
                // title + hint + options
                1 + hint + options
            };
            // top sep + prompt region + bottom sep + status + page region
            return 1 + prompt_rows + 1 + 1 + page_extra_height(pages, active_page);
        }
        let input_rows = rendered_input_rows(input, terminal_width);
        // top sep + input_rows + bottom sep + status + page region
        1 + input_rows.max(1) + 1 + 1 + page_extra_height(pages, active_page)
    }

    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        tick: usize,
        prompt: Option<&Prompt>,
        pages: &[PanePage],
        active_page: usize,
        pane_toggle_hint: Option<&str>,
    ) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let sep = "─".repeat(area.width as usize);
        let bottom_sep = separator_with_hint(area.width, pane_toggle_hint);

        // Reserve rows from the bottom: status bar (1) + bottom sep (1) + page region
        let page_height = page_extra_height(pages, active_page);
        let content_bottom = area.bottom().saturating_sub(2 + page_height).max(area.y);

        let input_view = if prompt.is_some() {
            None
        } else {
            Some(rendered_input_rows(input, area.width))
        };

        let mut y = area.y;
        if area.height > 0 {
            frame.render_widget(
                Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
            y += 1;
        }

        if let Some(prompt) = prompt {
            let shown_options = prompt.visible_options();
            let shown_len = shown_options.len() as u16;
            let hint_rows = u16::from(prompt.hint.is_some());
            let options_top = content_bottom.saturating_sub(shown_len + hint_rows);
            if let Some(ref cmd) = prompt.full_command {
                let title = shell_prompt_title(prompt);
                let title_lines = wrap::wrap_text(&title, area.width as usize);
                for title_line in title_lines {
                    if y >= options_top {
                        break;
                    }
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            title_line,
                            Style::default().fg(self.theme.system_msg),
                        )),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }

                let cmd_visual_lines = shell_command_preview_lines(cmd, area.width as usize);
                let max_preview = if prompt.peek_mode {
                    cmd_visual_lines.len()
                } else {
                    cmd_visual_lines.len().min(COMMAND_PREVIEW_LINES)
                };

                for visual_line in cmd_visual_lines.iter().take(max_preview) {
                    if y >= options_top {
                        break;
                    }
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            visual_line.clone(),
                            Style::default().fg(self.theme.tool_call),
                        )),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }

                // Combined hint/truncation line (only shown when truncated or in peek mode)
                if y < options_top
                    && (prompt.peek_mode || cmd_visual_lines.len() > COMMAND_PREVIEW_LINES)
                {
                    let hint = if prompt.peek_mode {
                        "    Press P to hide full command".to_string()
                    } else {
                        let remaining = cmd_visual_lines.len() - COMMAND_PREVIEW_LINES;
                        format!(
                            "    … [+{} more lines]  Press P to show full command",
                            remaining
                        )
                    };
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            hint,
                            Style::default().fg(self.theme.system_msg),
                        )),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }

                // Options
                y = y.max(options_top);
                for i in shown_options.clone() {
                    if y >= content_bottom {
                        break;
                    }
                    let option = &prompt.options[i];
                    let selected = i == prompt.selected;
                    let (marker, marker_style) = if selected {
                        (
                            ">",
                            Style::default()
                                .fg(self.theme.status_text)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        (" ", Style::default().fg(ratatui::style::Color::DarkGray))
                    };
                    let option_style = Style::default().fg(ratatui::style::Color::White);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("  {} ", marker), marker_style),
                            Span::styled(option.clone(), option_style),
                        ])),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }
            } else {
                // Title line
                if y < options_top {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            format!("  {}", prompt.title),
                            Style::default().fg(self.theme.system_msg),
                        )),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }

                // Options — one per line
                y = y.max(options_top);
                for i in shown_options {
                    if y >= content_bottom {
                        break;
                    }
                    let option = &prompt.options[i];
                    let selected = i == prompt.selected;
                    let (marker, marker_style) = if selected {
                        (
                            ">",
                            Style::default()
                                .fg(self.theme.status_text)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        (" ", Style::default().fg(ratatui::style::Color::DarkGray))
                    };
                    let option_style = Style::default().fg(ratatui::style::Color::White);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("  {} ", marker), marker_style),
                            Span::styled(option.clone(), option_style),
                        ])),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }
            }
            if let Some(hint) = &prompt.hint
                && y < content_bottom
            {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  {hint}"),
                        Style::default().fg(self.theme.system_msg),
                    )),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
            }
        } else if let Some(input_rows) = input_view {
            let visible_input_rows = input_rows.min(content_bottom.saturating_sub(y));
            if visible_input_rows > 0 {
                frame.render_widget(
                    Paragraph::new(input_text(input)).wrap(Wrap { trim: false }),
                    Rect {
                        y,
                        height: visible_input_rows,
                        width: area.width.saturating_sub(1).max(1),
                        ..area
                    },
                );
            }
        }

        // ── Page region ──────────────────────────────────────────────────
        if !pages.is_empty() {
            // Bottom separator is at area.bottom()-2, status at area.bottom()-1.
            // Page region sits between the input/prompt area and those two rows.
            // We need at least 1 row for the page separator.
            let status_row = area.bottom().saturating_sub(1);
            let bottom_sep_row = area.bottom().saturating_sub(2);

            // Earliest row we can use for the page region
            let page_start = content_bottom.max(area.y);
            // How many rows are actually available between content_bottom and bottom_sep_row
            let available = bottom_sep_row.saturating_sub(page_start);

            if available == 0 {
                // No room for pages at all — skip
            } else if pages.len() > 1 {
                // Multi-page: [sep] [content...] [sep] [tab indicator]
                // Reserve: 1 (top sep) + content + 1 (bottom sep) + 1 (tab) = content + 3
                let tab_rows: u16 = 1;
                let sep_rows: u16 = 2; // top + bottom
                let content_available = available.saturating_sub(sep_rows + tab_rows);
                let content_rows = (page_visible_rows(&pages[active_page.min(pages.len() - 1)])
                    as u16)
                    .min(content_available);
                let mut py = page_start;

                // Page top separator
                frame.render_widget(
                    Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
                    Rect {
                        y: py,
                        height: 1,
                        ..area
                    },
                );
                py += 1;

                // Page content
                let page_idx = active_page.min(pages.len() - 1);
                let page = &pages[page_idx];
                let scroll = page.scroll.min(page.content.len());
                for line in page.content.iter().skip(scroll).take(content_rows as usize) {
                    if py >= bottom_sep_row {
                        break;
                    }
                    frame.render_widget(
                        Paragraph::new(line.clone()),
                        Rect {
                            y: py,
                            height: 1,
                            ..area
                        },
                    );
                    py += 1;
                }
                // Fill remaining content rows if scroll skipped some
                while py < page_start + 1 + content_rows && py < bottom_sep_row {
                    py += 1;
                }

                // Page bottom separator
                let bot_sep_y = page_start + 1 + content_rows;
                if bot_sep_y < bottom_sep_row {
                    frame.render_widget(
                        Paragraph::new(sep.clone())
                            .style(Style::default().fg(self.theme.input_border)),
                        Rect {
                            y: bot_sep_y,
                            height: 1,
                            ..area
                        },
                    );
                }

                // Tab indicator
                let tab_y = bot_sep_y + 1;
                if tab_y < status_row {
                    let mut tab_spans = Vec::new();
                    for (i, p) in pages.iter().enumerate() {
                        if i > 0 {
                            tab_spans.push(Span::styled(
                                " | ",
                                Style::default().fg(self.theme.system_msg),
                            ));
                        }
                        if i == page_idx {
                            tab_spans.push(Span::styled(
                                p.title.clone(),
                                Style::default()
                                    .fg(self.theme.status_text)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            tab_spans.push(Span::styled(
                                p.title.clone(),
                                Style::default().fg(self.theme.system_msg),
                            ));
                        }
                    }
                    tab_spans.push(Span::styled(
                        "  Tab to switch",
                        Style::default().fg(ratatui::style::Color::DarkGray),
                    ));
                    frame.render_widget(
                        Paragraph::new(Line::from(tab_spans)),
                        Rect {
                            y: tab_y,
                            height: 1,
                            ..area
                        },
                    );
                }
            } else {
                // Single page: [sep] [content...]. The fixed bottom separator
                // below the page region closes the pane.
                let sep_rows: u16 = 1;
                let content_available = available.saturating_sub(sep_rows);
                let content_rows = (page_visible_rows(&pages[0]) as u16).min(content_available);
                let mut py = page_start;

                // Page top separator
                frame.render_widget(
                    Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
                    Rect {
                        y: py,
                        height: 1,
                        ..area
                    },
                );
                py += 1;

                // Page content
                let page = &pages[0];
                let scroll = page.scroll.min(page.content.len());
                for line in page.content.iter().skip(scroll).take(content_rows as usize) {
                    if py >= bottom_sep_row {
                        break;
                    }
                    frame.render_widget(
                        Paragraph::new(line.clone()),
                        Rect {
                            y: py,
                            height: 1,
                            ..area
                        },
                    );
                    py += 1;
                }
            }
        }

        // ── Bottom separator ─────────────────────────────────────────────
        if area.height >= 2 {
            frame.render_widget(
                Paragraph::new(bottom_sep).style(Style::default().fg(self.theme.input_border)),
                Rect {
                    y: area.bottom() - 2,
                    height: 1,
                    ..area
                },
            );
        }

        // ── Status bar ───────────────────────────────────────────────────
        let mut status_spans: Vec<Span> = vec![
            Span::styled(
                status_info.model.to_string(),
                Style::default().fg(self.theme.status_text),
            ),
            Span::styled(" | ", Style::default().fg(self.theme.status_text)),
            Span::styled(
                status_info.approval_mode.label().to_string(),
                Style::default().fg(match status_info.approval_mode {
                    ApprovalMode::Safe => self.theme.approval_safe,
                    ApprovalMode::Edits => self.theme.approval_edits,
                    ApprovalMode::Danger => self.theme.approval_danger,
                }),
            ),
            Span::styled(" | ", Style::default().fg(self.theme.status_text)),
            Span::styled(
                status_info
                    .token_stats
                    .display_with_received_override(status_info.streaming_completion_tokens),
                Style::default().fg(self.theme.status_text),
            ),
        ];

        if status_info.queue_len > 0 {
            status_spans.push(Span::styled(
                " | ",
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(Span::styled(
                format!("Q: {}", status_info.queue_len),
                Style::default().fg(self.theme.status_text),
            ));
        }

        if status_info.streaming {
            status_spans.push(Span::styled(
                " | ",
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(Span::styled(
                SPINNER[tick % SPINNER.len()],
                Style::default().fg(self.theme.thinking),
            ));
            status_spans.push(Span::styled(
                " thinking",
                Style::default().fg(self.theme.status_text),
            ));
        }

        if area.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(status_spans)),
                Rect {
                    y: area.bottom() - 1,
                    height: 1,
                    ..area
                },
            );
        }
    }
}
