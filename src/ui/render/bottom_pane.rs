use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::wrap;
use super::{InputState, Prompt, SPINNER, StatusInfo};
use crate::tools::ApprovalMode;
use crate::ui::autocomplete::{AutocompleteState, MAX_VISIBLE};
use crate::ui::pane_page::PanePage;
use crate::ui::tool_display;

/// Arguments shared by pane-drawing methods.
pub struct PaneDraw<'a> {
    pub input: &'a InputState,
    pub status_info: &'a StatusInfo,
    pub pages: &'a [PanePage],
    pub active_page: usize,
    pub autocomplete: Option<&'a AutocompleteState>,
}
fn build_tab_bar(items: &[String], active_idx: usize, separator: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, label) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                format!(" {separator} "),
                Style::default().fg(ratatui::style::Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            label.clone(),
            if i == active_idx {
                Style::default()
                    .fg(ratatui::style::Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ratatui::style::Color::DarkGray)
            },
        ));
    }
    Line::from(spans)
}

fn push_metric(parts: &mut Vec<Span<'static>>, style: Style, label: &str) {
    if !parts.is_empty() {
        parts.push(Span::styled(" / ", style));
    }
    parts.push(Span::styled(label.to_string(), style));
}

const COMMAND_PREVIEW_LINES: usize = 6;
const BLANK_CURSOR_CELL: &str = "\u{00a0}";

/// Default rows of pane page content visible at once.
pub const DEFAULT_PANE_ROWS: usize = 8;
/// Upper safety cap for rows of pane page content visible at once.
pub const MAX_PANE_ROWS: usize = 24;

/// Pre-computed layout for the page region of the bottom pane.
/// Shared by `desired_height` (unclamped) and drawing (clamped to available space).
struct PageLayout {
    /// Number of content rows that will be rendered.
    content_rows: u16,
    /// Total height: top sep + content.
    total_height: u16,
}

impl PageLayout {
    /// Compute layout. `max_content` caps content rows; pass `u16::MAX` for unclamped.
    fn compute(pages: &[PanePage], active_page: usize, max_content: u16) -> Self {
        if pages.is_empty() {
            return Self {
                content_rows: 0,
                total_height: 0,
            };
        }
        let page_idx = active_page.min(pages.len() - 1);
        let wanted = page_visible_rows(&pages[page_idx]) as u16;
        let content_rows = wanted.min(max_content);
        // Chrome: 1 top sep
        let chrome: u16 = 1;
        Self {
            content_rows,
            total_height: chrome.saturating_add(content_rows),
        }
    }
}

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

fn prompt_option_line(
    theme: &crate::ui::theme::Theme,
    option: &str,
    selected: bool,
) -> Line<'static> {
    let marker_style = if selected {
        Style::default()
            .fg(theme.thinking)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ratatui::style::Color::DarkGray)
    };
    let text_style = if selected {
        Style::default()
            .fg(ratatui::style::Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ratatui::style::Color::DarkGray)
    };
    let muted_style = Style::default().fg(theme.status_text);
    let good_style = Style::default().fg(theme.approval_safe);

    let (marker, marker_style) = if selected {
        ("›", marker_style)
    } else {
        (" ", marker_style)
    };

    let mut spans = vec![Span::styled(format!("  {marker} "), marker_style)];
    spans.extend(styled_circle_option_spans(
        option,
        text_style,
        muted_style,
        good_style,
    ));
    Line::from(spans)
}

fn push_prompt_text_spans(
    text: &str,
    text_style: Style,
    muted_style: Style,
    spans: &mut Vec<Span<'static>>,
) {
    let mut first = true;
    for part in text.split(" · ") {
        if !first {
            spans.push(Span::styled(" · ", muted_style));
        }
        spans.push(Span::styled(
            part.to_string(),
            if first { text_style } else { muted_style },
        ));
        first = false;
    }
}

fn styled_circle_option_spans(
    option: &str,
    text_style: Style,
    muted_style: Style,
    good_style: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some(rest) = option.strip_prefix("● ") {
        // green filled circle: value is active/true
        spans.push(Span::styled("● ", good_style));
        push_prompt_text_spans(rest, text_style, muted_style, &mut spans);
    } else if let Some(rest) = option.strip_prefix("○ ") {
        // empty circle: value is inactive/false
        spans.push(Span::styled("○ ", muted_style));
        push_prompt_text_spans(rest, text_style, muted_style, &mut spans);
    } else {
        push_prompt_text_spans(option, text_style, muted_style, &mut spans);
    }
    spans
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

/// Clamp a tool-requested pane content height to the supported range.
pub(crate) fn clamped_pane_visible_rows(visible_rows: usize) -> usize {
    visible_rows.clamp(1, MAX_PANE_ROWS)
}

/// Compute how many rows a page's visible content occupies.
fn page_visible_rows(page: &PanePage) -> usize {
    let requested = clamped_pane_visible_rows(page.visible_rows);
    let content_rows = page.content.len().saturating_sub(page.scroll);
    content_rows.min(requested)
}

/// Compute extra height needed for the page region (separators + content +
/// tab indicator).
fn page_extra_height(pages: &[PanePage], active_page: usize) -> u16 {
    PageLayout::compute(pages, active_page, u16::MAX).total_height
}

impl super::Renderer {
    /// Draw the bottom pane into the fixed inline viewport.
    pub fn draw_bottom_pane(
        &self,
        frame: &mut Frame,
        args: &PaneDraw<'_>,
        prompt: Option<&Prompt>,
    ) {
        self.draw_bottom_pane_with_tick(frame, args, self.spinner_tick, prompt);
    }

    /// Compute the desired viewport height for the current state.
    pub fn desired_height(
        input: &InputState,
        prompt: Option<&Prompt>,
        terminal_width: u16,
        pages: &[PanePage],
        active_page: usize,
        autocomplete: Option<&AutocompleteState>,
    ) -> u16 {
        if let Some(p) = prompt {
            let options = p.options.len().min(p.visible_rows) as u16;
            let hint = u16::from(p.hint.is_some());
            let tab_bar = u16::from(!p.tabs.is_empty());
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
                // title (only without tabs) + hint + options
                let title = u16::from(p.tabs.is_empty());
                title + hint + options
            };
            // top sep + tab bar + prompt region + status + page region
            return 1 + tab_bar + prompt_rows + 1 + page_extra_height(pages, active_page);
        }
        let input_rows = rendered_input_rows(input, terminal_width);
        let ac_rows = autocomplete.map(|ac| ac.visible_rows()).unwrap_or(0);
        // top sep + input_rows + autocomplete + bot_sep + status + page region
        let bot_sep = u16::from(pages.is_empty());
        1 + input_rows.max(1) + ac_rows + bot_sep + 1 + page_extra_height(pages, active_page)
    }

    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        args: &PaneDraw<'_>,
        tick: usize,
        prompt: Option<&Prompt>,
    ) {
        let input = args.input;
        let status_info = args.status_info;
        let pages = args.pages;
        let active_page = args.active_page;
        let ac = args.autocomplete;
        let area = frame.area();
        frame.render_widget(Clear, area);
        let sep = "─".repeat(area.width as usize);

        // Reserve rows from the bottom: status bar (1) + page region
        let page_height = page_extra_height(pages, active_page);
        let ac_rows = ac.map(|a| a.visible_rows()).unwrap_or(0);
        let content_bottom = area.bottom().saturating_sub(1 + page_height).max(area.y);

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

        // ── Tab bar (for multi-section prompts like /config) ─────────────
        if let Some(prompt) = prompt
            && !prompt.tabs.is_empty()
        {
            let tab_y = y;
            if tab_y < content_bottom {
                let tab_bar = build_tab_bar(&prompt.tabs, prompt.active_tab, "│");
                frame.render_widget(
                    Paragraph::new(tab_bar),
                    Rect {
                        y: tab_y,
                        height: 1,
                        ..area
                    },
                );
                y += 1;
            }
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
                    frame.render_widget(
                        Paragraph::new(prompt_option_line(&self.theme, option, selected)),
                        Rect {
                            y,
                            height: 1,
                            ..area
                        },
                    );
                    y += 1;
                }
            } else {
                // Title line — skip when tabs are shown (tab bar already displays it)
                let show_title = prompt.tabs.is_empty();
                if show_title && y < options_top {
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
                    frame.render_widget(
                        Paragraph::new(prompt_option_line(&self.theme, option, selected)),
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
                let hint_text = format!("  {hint}");
                let display_width = UnicodeWidthStr::width(hint_text.as_str());
                let padded = format!(
                    "{hint_text}{}",
                    " ".repeat((area.width as usize).saturating_sub(display_width))
                );
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        padded,
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
            y += visible_input_rows;
        }

        // ── Autocomplete dropdown ──────────────────────────────────────
        if let Some(ac) = ac {
            let ac_end = y + ac_rows;
            let name_width = ac.max_name_width();
            for (local_i, (name, desc)) in ac
                .matches
                .iter()
                .skip(ac.scroll_offset)
                .take(MAX_VISIBLE)
                .enumerate()
            {
                if y >= ac_end {
                    break;
                }
                let i = ac.scroll_offset + local_i;
                let selected = i == ac.selected;
                let style = if selected {
                    Style::default()
                        .fg(ratatui::style::Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.theme.status_text)
                };
                let label = format!("  /{name:<name_width$} — {desc}");
                frame.render_widget(
                    Paragraph::new(Span::styled(label, style)),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
                y += 1;
            }
            if ac.more_count() > 0 && y < ac_end {
                let hint = format!("  … [+{} more]", ac.more_count());
                let display_width = UnicodeWidthStr::width(hint.as_str());
                let padded = format!(
                    "{hint}{}",
                    " ".repeat((area.width as usize).saturating_sub(display_width))
                );
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        padded,
                        Style::default().fg(self.theme.status_text),
                    )),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
            }
        }

        // Input bottom border — persists even when panes are hidden (Ctrl+T).
        // When pages are visible, the page top separator serves this role.
        if pages.is_empty() && input_view.is_some() && y < content_bottom {
            frame.render_widget(
                Paragraph::new(sep.clone()).style(Style::default().fg(self.theme.input_border)),
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
        }

        // ── Page region ──────────────────────────────────────────────────
        if !pages.is_empty() {
            let bottom_sep_row = area.bottom().saturating_sub(1);
            let page_start = content_bottom.max(area.y);
            let available = bottom_sep_row.saturating_sub(page_start);

            let chrome: u16 = 1;
            let max_content = available.saturating_sub(chrome);
            let layout = PageLayout::compute(pages, active_page, max_content);

            if available > 0 {
                let page_idx = active_page.min(pages.len() - 1);

                let page_sep = "─".repeat(area.width as usize);
                frame.render_widget(
                    Paragraph::new(page_sep).style(Style::default().fg(self.theme.input_border)),
                    Rect {
                        y: page_start,
                        height: 1,
                        ..area
                    },
                );

                // Page content
                if layout.content_rows > 0 {
                    let page = &pages[page_idx];
                    let scroll = page.scroll.min(page.content.len());

                    for (py, line) in (page_start + 1..).zip(
                        page.content
                            .iter()
                            .skip(scroll)
                            .take(layout.content_rows as usize),
                    ) {
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
                    }
                }
            }
        }

        // ── Status bar ───────────────────────────────────────────────────
        let mut status_spans: Vec<Span> = vec![];
        let sep = || Span::styled(" | ", Style::default().fg(self.theme.status_text));

        if status_info.show("status_show_model") {
            status_spans.push(Span::styled(
                status_info.model.to_string(),
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(sep());
        }

        if status_info.show("status_show_approval") {
            status_spans.push(Span::styled(
                status_info.approval_mode.label().to_string(),
                Style::default().fg(match status_info.approval_mode {
                    ApprovalMode::Safe => self.theme.approval_safe,
                    ApprovalMode::Danger => self.theme.approval_danger,
                }),
            ));
            status_spans.push(sep());
        }

        use crate::llm::token_tracker::format_tokens;

        let received = status_info
            .streaming_completion_tokens
            .unwrap_or(status_info.token_stats.received);
        let any_token_metric = status_info.show("status_show_tokens_curr")
            || status_info.show("status_show_tokens_in")
            || status_info.show("status_show_tokens_out")
            || status_info.show("status_show_tokens_total");

        if any_token_metric {
            let mut metric_parts: Vec<Span> = vec![];
            let s = Style::default().fg(self.theme.status_text);
            if status_info.show("status_show_tokens_curr") {
                push_metric(
                    &mut metric_parts,
                    s,
                    &format!(
                        "curr {}",
                        format_tokens(status_info.token_stats.context_length)
                    ),
                );
            }
            if status_info.show("status_show_tokens_in") {
                push_metric(
                    &mut metric_parts,
                    s,
                    &format!("in {}", format_tokens(status_info.token_stats.sent)),
                );
            }
            if status_info.show("status_show_tokens_out") {
                push_metric(
                    &mut metric_parts,
                    s,
                    &format!("out {}", format_tokens(received)),
                );
            }
            if status_info.show("status_show_tokens_total") {
                push_metric(
                    &mut metric_parts,
                    s,
                    &format!(
                        "total {}",
                        format_tokens(status_info.token_stats.sent + received)
                    ),
                );
            }
            status_spans.extend(metric_parts);
            status_spans.push(sep());
        }

        if status_info.show("status_show_queue") && status_info.queue_len > 0 {
            status_spans.push(Span::styled(
                format!("Q: {}", status_info.queue_len),
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(sep());
        }

        if status_info.show("status_show_timer")
            && let Some(ref elapsed) = status_info.elapsed
        {
            status_spans.push(Span::styled(
                elapsed.clone(),
                Style::default().fg(self.theme.status_text),
            ));
            status_spans.push(sep());
        }

        if status_info.show("status_show_spinner") && status_info.streaming {
            status_spans.push(Span::styled(
                SPINNER[tick % SPINNER.len()],
                Style::default().fg(self.theme.thinking),
            ));
            status_spans.push(Span::styled(
                " thinking",
                Style::default().fg(self.theme.status_text),
            ));
        }

        // Remove trailing separator if present
        if let Some(last) = status_spans.last()
            && last.content == " | "
        {
            status_spans.pop();
        }

        // Append Lua-defined status segments (`bone.api.ui.set_statusline`).
        // Left/center segments extend the native bar; right segments are drawn
        // right-aligned on the same row.
        use crate::runtime::view::Align;
        let seg_span = |seg: &crate::runtime::view::StatusSegment| {
            let color = seg
                .fg
                .as_deref()
                .and_then(crate::ui::color::parse_color)
                .unwrap_or(self.theme.status_text);
            Span::styled(seg.text.clone(), Style::default().fg(color))
        };
        let mut right_spans: Vec<Span> = vec![];
        for seg in &status_info.lua_status {
            if matches!(seg.align, Align::Right) {
                right_spans.push(seg_span(seg));
            } else {
                if !status_spans.is_empty() {
                    status_spans.push(sep());
                }
                status_spans.push(seg_span(seg));
            }
        }

        if area.height > 0 {
            let row = Rect {
                y: area.bottom() - 1,
                height: 1,
                ..area
            };
            let right_line = Line::from(right_spans);
            // Reserve the right-aligned segments' width so they never overwrite
            // the left/native content on the same row.
            let right_width = right_line.width() as u16;
            let left_row = if right_width > 0 {
                Rect {
                    width: row.width.saturating_sub(right_width + 1),
                    ..row
                }
            } else {
                row
            };
            frame.render_widget(Paragraph::new(Line::from(status_spans)), left_row);
            if right_width > 0 {
                frame.render_widget(
                    Paragraph::new(right_line).alignment(ratatui::layout::Alignment::Right),
                    row,
                );
            }
        }
    }
}
