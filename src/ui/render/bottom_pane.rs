use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::wrap;
use super::{InputState, Prompt, SPINNER, StatusInfo};
use crate::tools::ApprovalMode;
use crate::ui::tool_display;

const COMMAND_PREVIEW_LINES: usize = 6;

fn bash_prompt_title(prompt: &Prompt) -> String {
    format!(
        "  {}",
        prompt.title.split(" — ").next().unwrap_or(&prompt.title)
    )
}

fn bash_command_preview_lines(command: &str, width: usize) -> Vec<String> {
    tool_display::format_bash_command(command)
        .into_iter()
        .flat_map(|line| wrap::wrap_text_with_prefix(&line, "  ", "  ", width))
        .collect()
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

impl super::Renderer {
    /// Draw the bottom pane into the fixed inline viewport.
    pub fn draw_bottom_pane(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        prompt: Option<&Prompt>,
    ) {
        self.draw_bottom_pane_with_tick(frame, input, status_info, self.spinner_tick, prompt);
    }

    /// Compute the desired viewport height for the current state.
    pub fn desired_height(input: &InputState, prompt: Option<&Prompt>, terminal_width: u16) -> u16 {
        if let Some(p) = prompt {
            if let Some(ref cmd) = p.full_command {
                let title = bash_prompt_title(p);
                let title_lines = wrap::wrap_text(&title, terminal_width as usize).len() as u16;
                let cmd_visual_lines =
                    bash_command_preview_lines(cmd, terminal_width as usize).len() as u16;
                let options = p.options.len() as u16;
                if p.peek_mode {
                    // sep + title + cmd_lines + hint + options + sep + status
                    return 1 + title_lines + cmd_visual_lines + 1 + options + 1 + 1;
                }
                // sep + title + preview + combined hint + options + sep + status
                let preview = cmd_visual_lines.min(COMMAND_PREVIEW_LINES as u16);
                return 1 + title_lines + preview + 1 + options + 1 + 1;
            }
            // sep + title + options + sep + status
            return 1 + 1 + p.options.len() as u16 + 1 + 1;
        }
        let (before, at_cursor, after) = cursor_split(input);
        let display = format!("> {}{}{}", before, at_cursor, after);
        let input_rows = wrap::visual_line_count(&display, terminal_width as usize) as u16;
        // sep + input_rows + sep + status
        return 1 + input_rows.max(1) + 1 + 1;
    }

    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        input: &InputState,
        status_info: &StatusInfo,
        tick: usize,
        prompt: Option<&Prompt>,
    ) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let sep = "─".repeat(area.width as usize);

        let input_view = if prompt.is_some() {
            None
        } else {
            let (before, at_cursor, after) = cursor_split(input);
            let display_text = format!("> {}{}{}", before, at_cursor, after);
            let rows = wrap::visual_line_count(&display_text, area.width as usize) as u16;
            Some((before, at_cursor, after, rows.max(1)))
        };

        let mut y = area.y;

        frame.render_widget(
            Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        y += 1;

        if let Some(prompt) = prompt {
            if let Some(ref cmd) = prompt.full_command {
                // ── Bash command preview ──

                let title = bash_prompt_title(prompt);
                let title_lines = wrap::wrap_text(&title, area.width as usize);
                for title_line in title_lines {
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

                let cmd_visual_lines = bash_command_preview_lines(cmd, area.width as usize);
                let max_preview = if prompt.peek_mode {
                    cmd_visual_lines.len()
                } else {
                    cmd_visual_lines.len().min(COMMAND_PREVIEW_LINES)
                };

                for visual_line in cmd_visual_lines.iter().take(max_preview) {
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

                // Combined hint/truncation line
                let hint = if prompt.peek_mode {
                    "    Press P to hide full command".to_string()
                } else if cmd_visual_lines.len() > COMMAND_PREVIEW_LINES {
                    let remaining = cmd_visual_lines.len() - COMMAND_PREVIEW_LINES;
                    format!(
                        "    … [+{} more lines]  Press P to show full command",
                        remaining
                    )
                } else {
                    "    Press P to show full command".to_string()
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

                // Options
                for (i, option) in prompt.options.iter().enumerate() {
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
                // ── Original non-command prompt ──

                // Title line
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

                // Options — one per line
                for (i, option) in prompt.options.iter().enumerate() {
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
        } else if let Some((before, at_cursor, after, input_rows)) = input_view {
            let input_line = Line::from(vec![
                Span::raw("> "),
                Span::raw(before),
                Span::styled(
                    at_cursor.to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ),
                Span::raw(after),
            ]);

            frame.render_widget(
                Paragraph::new(input_line).wrap(Wrap { trim: false }),
                Rect {
                    y,
                    height: input_rows,
                    ..area
                },
            );
            y += input_rows;
        }

        frame.render_widget(
            Paragraph::new(sep).style(Style::default().fg(self.theme.input_border)),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        y += 1;

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

        frame.render_widget(
            Paragraph::new(Line::from(status_spans)),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}
