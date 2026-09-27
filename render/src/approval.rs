//! The tool-approval prompt: an Accept / Advise / Cancel choice with a shell
//! command preview, drawn in the live pane by every frontend.

use bone_protocol::ToolCall;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::prompt::Prompt;
use crate::{tool_display, wrap};

pub const COMMAND_PREVIEW_LINES: usize = 6;

/// The prompt for approving `call`: its title names the tool and target, and a
/// shell call carries its full command for the preview.
pub fn approval_prompt(call: &ToolCall) -> Prompt {
    let summary = match call.name.as_str() {
        "read_file" | "create_file" | "edit_file" => {
            call.arguments["path"].as_str().unwrap_or("?").to_string()
        }
        "shell" => call.arguments["command"]
            .as_str()
            .unwrap_or("?")
            .to_string(),
        _ => call.name.clone(),
    };
    let is_shell = call.name == "shell";
    let title = if is_shell {
        call.arguments["display_label"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                call.arguments["command"]
                    .as_str()
                    .unwrap_or("?")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>()
            })
    } else {
        summary
    };
    let mut prompt = Prompt::new(
        format!("{} — {}", call.name, title),
        vec!["Accept", "Advise", "Cancel"],
    );
    prompt.full_command = if is_shell {
        call.arguments["command"].as_str().map(String::from)
    } else {
        None
    };
    prompt
}

/// The reply sent for advice typed at an approval prompt.
pub fn advice_reply(advice: &str) -> String {
    format!("[exit_code=1] Tool not executed. User advice: {advice}")
}

pub fn shell_prompt_title(prompt: &Prompt) -> String {
    format!(
        "  {}",
        prompt.title.split(" — ").next().unwrap_or(&prompt.title)
    )
}

pub fn shell_command_preview_lines(command: &str, width: usize) -> Vec<String> {
    tool_display::format_shell_command(command)
        .into_iter()
        .flat_map(|line| wrap::wrap_text_with_prefix(&line, "  ", "  ", width))
        .collect()
}

pub fn prompt_option_line(
    theme: &crate::theme::Theme,
    option: &str,
    selected: bool,
) -> Line<'static> {
    let marker_style = if selected {
        Style::default()
            .fg(theme.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.palette.muted)
    };
    let text_style = if selected {
        Style::default()
            .fg(theme.palette.fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.palette.muted)
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
pub fn approval_pane_lines(
    theme: &crate::theme::Theme,
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

pub fn push_prompt_text_spans(
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

pub fn styled_circle_option_spans(
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
