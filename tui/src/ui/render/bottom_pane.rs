//! Bottom-pane rendering: input box, prompt, status bar, and tool pages.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::wrap;
use super::{InputState, Prompt, StatusInfo};
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
    /// In-flight shell commands (call_id, formatted label, start time), shown as
    /// a transient strip above the input while running.
    pub running: &'a [(String, String, std::time::Instant)],
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

const MAX_INPUT_PADDING: u16 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPreset {
    Lines,
    Box,
    Filled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputBorder {
    horizontal: String,
    vertical: String,
    top_left: String,
    top_right: String,
    bottom_left: String,
    bottom_right: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputStyle {
    pub preset: InputPreset,
    pub prefix: String,
    pub horizontal_padding: u16,
    pub vertical_padding: u16,
    pub fill: bool,
    border: InputBorder,
}

impl Default for InputStyle {
    fn default() -> Self {
        Self::preset(InputPreset::Lines)
    }
}

impl InputStyle {
    fn preset(preset: InputPreset) -> Self {
        let (horizontal_padding, vertical_padding, fill) = match preset {
            InputPreset::Lines => (0, 0, false),
            InputPreset::Box => (1, 0, false),
            InputPreset::Filled => (1, 1, true),
        };
        Self {
            preset,
            prefix: "> ".to_string(),
            horizontal_padding,
            vertical_padding,
            fill,
            border: InputBorder {
                horizontal: "─".to_string(),
                vertical: "│".to_string(),
                top_left: "╭".to_string(),
                top_right: "╮".to_string(),
                bottom_left: "╰".to_string(),
                bottom_right: "╯".to_string(),
            },
        }
    }

    pub fn from_snapshot(snapshot: &crate::ext::snapshots::InputStyleSnapshot) -> Self {
        let preset = match snapshot.preset.as_deref() {
            None | Some("lines") => InputPreset::Lines,
            Some("box") => InputPreset::Box,
            Some("filled") => InputPreset::Filled,
            Some(other) => {
                bone_core::ext::ctx::runtime_warn_once(format!(
                    "bone-lua warn: invalid input preset '{other}'; using lines"
                ));
                InputPreset::Lines
            }
        };
        let mut style = Self::preset(preset);
        if snapshot.show_prefix == Some(false) {
            style.prefix.clear();
        } else if let Some(prefix) = &snapshot.prefix {
            style.prefix = prefix.clone();
        }
        if let Some(padding) = snapshot.horizontal_padding {
            style.horizontal_padding = padding.min(MAX_INPUT_PADDING);
        }
        if let Some(padding) = snapshot.vertical_padding {
            style.vertical_padding = padding.min(MAX_INPUT_PADDING);
        }
        if let Some(fill) = snapshot.fill {
            style.fill = fill;
        }

        fn glyph(value: &Option<String>, fallback: &str) -> String {
            value
                .as_deref()
                .filter(|value| UnicodeWidthStr::width(*value) == 1)
                .unwrap_or(fallback)
                .to_string()
        }
        style.border = InputBorder {
            horizontal: glyph(&snapshot.border.horizontal, &style.border.horizontal),
            vertical: glyph(&snapshot.border.vertical, &style.border.vertical),
            top_left: glyph(&snapshot.border.top_left, &style.border.top_left),
            top_right: glyph(&snapshot.border.top_right, &style.border.top_right),
            bottom_left: glyph(&snapshot.border.bottom_left, &style.border.bottom_left),
            bottom_right: glyph(&snapshot.border.bottom_right, &style.border.bottom_right),
        };
        style
    }

    fn has_sides(&self) -> bool {
        self.preset == InputPreset::Box
    }

    fn top_border(&self) -> bool {
        self.preset != InputPreset::Filled
    }

    fn bottom_border(&self) -> bool {
        match self.preset {
            InputPreset::Lines | InputPreset::Box => true,
            InputPreset::Filled => false,
        }
    }

    fn input_width(&self, terminal_width: u16) -> u16 {
        let sides = u16::from(self.has_sides()).saturating_mul(2);
        terminal_width
            .saturating_sub(1 + sides + self.horizontal_padding.saturating_mul(2))
            .max(1)
    }

    fn content_rect(&self, area: Rect, y: u16, height: u16) -> Rect {
        let side = u16::from(self.has_sides());
        let inset = side.saturating_add(self.horizontal_padding);
        let x_offset = inset.min(area.width.saturating_sub(1));
        let available = area
            .width
            .saturating_sub(1 + x_offset + side + self.horizontal_padding);
        Rect {
            x: area.x.saturating_add(x_offset),
            y,
            width: available,
            height,
        }
    }
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

fn running_elapsed(started_at: std::time::Instant) -> String {
    let elapsed = started_at.elapsed();
    if elapsed.as_secs() < 60 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{}m {:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    }
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut width = 0;
    let mut truncated = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width >= max_width {
            break;
        }
        truncated.push(ch);
        width += ch_width;
    }
    truncated.push('…');
    truncated
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

/// Build the styled content lines for the tool-approval prompt rendered as a
/// live pane (consistent with `/config` and other interactive menus, which all
/// live in the pane region). Mirrors the title/command/option styling the old
/// input-slot prompt used, so the move is visual-only. `width` is the pane's
/// render width, used to wrap the shell command preview.
pub(crate) fn approval_pane_lines(
    theme: &crate::ui::theme::Theme,
    prompt: &Prompt,
    advising: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Title: tool name + summary (for shell, the short label).
    let title = if prompt.full_command.is_some() {
        shell_prompt_title(prompt)
    } else {
        format!("  {}", prompt.title)
    };
    lines.push(Line::from(Span::styled(
        title,
        Style::default().fg(theme.system_msg),
    )));

    // Shell command preview, respecting peek mode.
    if let Some(ref cmd) = prompt.full_command {
        let cmd_lines = shell_command_preview_lines(cmd, width as usize);
        let max_preview = if prompt.peek_mode {
            cmd_lines.len()
        } else {
            cmd_lines.len().min(COMMAND_PREVIEW_LINES)
        };
        for visual_line in cmd_lines.iter().take(max_preview) {
            lines.push(Line::from(Span::styled(
                visual_line.clone(),
                Style::default().fg(theme.tool_call),
            )));
        }
        if prompt.peek_mode {
            lines.push(Line::from(Span::styled(
                "    Press P to hide full command".to_string(),
                Style::default().fg(theme.system_msg),
            )));
        } else if cmd_lines.len() > COMMAND_PREVIEW_LINES {
            let remaining = cmd_lines.len() - COMMAND_PREVIEW_LINES;
            lines.push(Line::from(Span::styled(
                format!("    … [+{remaining} more lines]  Press P to show full command"),
                Style::default().fg(theme.system_msg),
            )));
        }
    }

    if advising {
        // Free-form advice mode: the user types into the chat input field
        // (rendered above the status bar); the pane shows the instruction.
        lines.push(Line::from(Span::styled(
            "  Type advice below · Enter to send · Esc to cancel".to_string(),
            Style::default().fg(theme.status_text),
        )));
    } else {
        for (i, option) in prompt.options.iter().enumerate() {
            lines.push(prompt_option_line(theme, option, i == prompt.selected));
        }
    }
    lines
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
fn input_text(
    input: &InputState,
    style: &InputStyle,
    theme: &crate::ui::theme::Theme,
) -> Text<'static> {
    let (before, at_cursor, after) = cursor_split(input);
    let mut spans = if style.prefix.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(
            style.prefix.clone(),
            Style::default().fg(theme.input_prefix),
        )]
    };
    let mut lines = Vec::new();

    push_input_text(&before, &mut spans, &mut lines);
    let cursor_style = Style::default()
        .fg(theme.input_cursor)
        .add_modifier(Modifier::REVERSED);
    if at_cursor == '\n' {
        spans.push(Span::styled(BLANK_CURSOR_CELL, cursor_style));
        lines.push(Line::from(std::mem::take(&mut spans)));
    } else {
        let cursor_cell = if at_cursor == ' ' {
            BLANK_CURSOR_CELL.to_string()
        } else {
            at_cursor.to_string()
        };
        spans.push(Span::styled(cursor_cell, cursor_style));
    }
    push_input_text(&after, &mut spans, &mut lines);
    lines.push(Line::from(spans));

    Text::from(lines)
}

fn rendered_input_rows(
    input: &InputState,
    terminal_width: u16,
    style: &InputStyle,
    theme: &crate::ui::theme::Theme,
) -> u16 {
    Paragraph::new(input_text(input, style, theme))
        .wrap(Wrap { trim: false })
        .line_count(style.input_width(terminal_width))
        .max(1) as u16
}

fn input_border_line(style: &InputStyle, width: u16, top: bool) -> String {
    if width == 0 {
        return String::new();
    }
    if style.has_sides() {
        let (left, right) = if top {
            (&style.border.top_left, &style.border.top_right)
        } else {
            (&style.border.bottom_left, &style.border.bottom_right)
        };
        if width == 1 {
            return left.clone();
        }
        format!(
            "{left}{}{right}",
            style
                .border
                .horizontal
                .repeat(width.saturating_sub(2) as usize)
        )
    } else {
        style.border.horizontal.repeat(width as usize)
    }
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
        self.draw_bottom_pane_with_tick(frame, args, prompt);
    }

    /// Compute the desired viewport height for the current state.
    pub fn desired_height(
        &self,
        input: &InputState,
        prompt: Option<&Prompt>,
        terminal_width: u16,
        pages: &[PanePage],
        active_page: usize,
        autocomplete: Option<&AutocompleteState>,
        running: usize,
    ) -> u16 {
        let running_rows = running as u16;
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
                1u16 + hint + options
            };
            // top sep + running + prompt region + status + page region
            return 1 + running_rows + prompt_rows + 1 + page_extra_height(pages, active_page);
        }
        let input_rows = rendered_input_rows(input, terminal_width, &self.input_style, &self.theme);
        let ac_rows = autocomplete.map(|ac| ac.visible_rows()).unwrap_or(0);
        let top = u16::from(self.input_style.top_border());
        let bottom = u16::from(self.input_style.bottom_border());
        let padding = self.input_style.vertical_padding.saturating_mul(2);
        running_rows
            + top
            + padding
            + input_rows.max(1)
            + ac_rows
            + bottom
            + 1
            + page_extra_height(pages, active_page)
    }

    pub fn draw_bottom_pane_with_tick(
        &self,
        frame: &mut Frame,
        args: &PaneDraw<'_>,
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
            Some(rendered_input_rows(
                input,
                area.width,
                &self.input_style,
                &self.theme,
            ))
        };

        let mut y = area.y;

        // ── Running shell commands strip (above the separator) ───────────
        if !args.running.is_empty() && y < content_bottom {
            let spinner = spinner_frame(status_info);
            let total = args.running.len();
            for (index, (_, label, started_at)) in args.running.iter().enumerate() {
                if y >= content_bottom {
                    break;
                }
                let first_line = label.lines().next().unwrap_or(label);
                let command = first_line.strip_prefix("shell ").unwrap_or(first_line);
                let mut spans = Vec::new();
                let mut prefix_width = 0;
                if let Some(ref s) = spinner {
                    prefix_width += UnicodeWidthStr::width(s.as_str()) + 1;
                    spans.push(Span::styled(
                        s.clone(),
                        Style::default().fg(self.theme.thinking),
                    ));
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    "RUNNING",
                    Style::default()
                        .fg(self.theme.thinking)
                        .add_modifier(Modifier::BOLD),
                ));
                prefix_width += 7;

                let elapsed = format!("  {}  ", running_elapsed(*started_at));
                prefix_width += UnicodeWidthStr::width(elapsed.as_str());
                spans.push(Span::styled(
                    elapsed,
                    Style::default().fg(self.theme.status_text),
                ));

                if total > 1 {
                    let position = format!("[{}/{}] ", index + 1, total);
                    prefix_width += UnicodeWidthStr::width(position.as_str());
                    spans.push(Span::styled(
                        position,
                        Style::default().fg(self.theme.status_text),
                    ));
                }

                let command = truncate_display_width(
                    command,
                    (area.width as usize).saturating_sub(prefix_width),
                );
                spans.extend(super::messages::shell_spans(&command, &self.theme));
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
                y += 1;
            }
        }

        if prompt.is_some() {
            if y < content_bottom {
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
        } else if let Some(input_rows) = input_view {
            let top = u16::from(self.input_style.top_border());
            let bottom = u16::from(self.input_style.bottom_border());
            let interior_height = self
                .input_style
                .vertical_padding
                .saturating_mul(2)
                .saturating_add(input_rows)
                .saturating_add(ac_rows);
            let composer_height = top
                .saturating_add(interior_height)
                .saturating_add(bottom)
                .min(content_bottom.saturating_sub(y));
            let composer_start = y;
            if self.input_style.fill && composer_height > 0 {
                frame.render_widget(
                    Paragraph::new("").style(Style::default().bg(self.theme.input_bg)),
                    Rect {
                        y,
                        height: composer_height,
                        ..area
                    },
                );
            }
            if self.input_style.has_sides() {
                let sides_start = composer_start.saturating_add(top);
                let sides_end = sides_start
                    .saturating_add(interior_height)
                    .min(content_bottom);
                for side_y in sides_start..sides_end {
                    for side_x in [area.x, area.right().saturating_sub(1)] {
                        frame.render_widget(
                            Paragraph::new(self.input_style.border.vertical.clone())
                                .style(Style::default().fg(self.theme.input_border)),
                            Rect {
                                x: side_x,
                                y: side_y,
                                width: u16::from(area.width > 0),
                                height: 1,
                            },
                        );
                    }
                }
            }
            if self.input_style.top_border() && y < content_bottom {
                frame.render_widget(
                    Paragraph::new(input_border_line(&self.input_style, area.width, true))
                        .style(Style::default().fg(self.theme.input_border)),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
                y += 1;
            }
            y = y.saturating_add(
                self.input_style
                    .vertical_padding
                    .min(content_bottom.saturating_sub(y)),
            );
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
                    Paragraph::new(input_text(input, &self.input_style, &self.theme))
                        .wrap(Wrap { trim: false }),
                    self.input_style.content_rect(area, y, visible_input_rows),
                );
            }
            y += visible_input_rows;
        }

        // Autocomplete is part of the styled composer, below the input text.
        if let (Some(ac), Some(_)) = (ac, input_view) {
            let ac_end = y.saturating_add(ac_rows).min(content_bottom);
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
                    self.input_style.content_rect(area, y, 1),
                );
                y += 1;
            }
            if ac.more_count() > 0 && y < ac_end {
                let hint = format!("  … [+{} more]", ac.more_count());
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        hint,
                        Style::default().fg(self.theme.status_text),
                    )),
                    self.input_style.content_rect(area, y, 1),
                );
            }
            y = ac_end;
        }

        if input_view.is_some() {
            y = y.saturating_add(
                self.input_style
                    .vertical_padding
                    .min(content_bottom.saturating_sub(y)),
            );
            if self.input_style.bottom_border() && y < content_bottom {
                frame.render_widget(
                    Paragraph::new(input_border_line(&self.input_style, area.width, false))
                        .style(Style::default().fg(self.theme.input_border)),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
            }
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

                frame.render_widget(
                    Paragraph::new(""),
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
                        // Carry a line-level background to the paragraph so it
                        // fills the full row width (edge-to-edge highlight, e.g.
                        // the selected row in /config).
                        let mut para = Paragraph::new(line.clone());
                        if let Some(bg) = line.style.bg {
                            para = para.style(Style::default().bg(bg));
                        }
                        frame.render_widget(
                            para,
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

        self.draw_status_bar(frame, status_info, area);
    }

    /// Draw the single-row status bar at the bottom of the viewport
    /// (`area.bottom() - 1`). Renders native segments (model, approval mode,
    /// token metrics, queue, timer, spinner) followed by Lua-defined segments;
    /// right-aligned Lua segments are drawn on the same row with their width
    /// reserved so they never overwrite the native/left content.
    fn draw_status_bar(&self, frame: &mut Frame, status_info: &StatusInfo, area: Rect) {
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
            let frames = &status_info.spinner_frames;
            if !frames.is_empty() {
                let speed = if status_info.spinner_speed_ms > 0 {
                    status_info.spinner_speed_ms
                } else {
                    80
                };
                let frame_idx = (status_info.spinner_elapsed_ms / speed) as usize % frames.len();
                status_spans.push(Span::styled(
                    frames[frame_idx].clone(),
                    Style::default().fg(self.theme.thinking),
                ));
                let texts = &status_info.spinner_texts;
                let label = if texts.is_empty() {
                    " thinking".to_string()
                } else if !status_info.spinner_text_rotate || texts.len() == 1 {
                    format!(" {}", texts[0])
                } else {
                    let cycle = match status_info
                        .spinner_elapsed_ms
                        .checked_div(status_info.spinner_text_speed_ms)
                    {
                        Some(c) => c as usize,
                        None => (status_info.spinner_elapsed_ms / speed) as usize / frames.len(),
                    };
                    let phrase = &texts[cycle % texts.len()];
                    format!(" {phrase}")
                };
                status_spans.push(Span::styled(
                    label,
                    Style::default().fg(self.theme.status_text),
                ));
            }
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

/// Current spinner frame string for the running-commands strip, mirroring
/// the status bar's spinner computation.
fn spinner_frame(status_info: &StatusInfo) -> Option<String> {
    let frames = &status_info.spinner_frames;
    if frames.is_empty() {
        return None;
    }
    let speed = if status_info.spinner_speed_ms > 0 {
        status_info.spinner_speed_ms
    } else {
        80
    };
    let idx = (status_info.spinner_elapsed_ms / speed) as usize % frames.len();
    Some(frames[idx].clone())
}
