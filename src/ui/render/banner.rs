use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use super::BoneTerminal;

/// Render startup banner into native scrollback above the inline viewport.
pub fn render(term: &mut BoneTerminal, provider: &str, model: &str) -> std::io::Result<()> {
    let term_width = term.size().map(|s| s.width).unwrap_or(80);
    let banner_lines = lines(provider, model, term_width);

    term.insert_before(banner_lines.len() as u16, |buf| {
        for (row, line) in banner_lines.iter().enumerate() {
            let area = ratatui::layout::Rect {
                x: 0,
                y: row as u16,
                width: buf.area.width,
                height: 1,
            };
            ratatui::widgets::Paragraph::new(line.clone()).render(area, buf);
        }
    })
}

fn padded_row(left: &str, right: &str, width: usize, ls: Style, rs: Style) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let pad = width.saturating_sub(UnicodeWidthStr::width(left) + UnicodeWidthStr::width(right));
    Line::from(vec![
        Span::styled("│ ", dim),
        Span::styled(left.to_string(), ls),
        Span::raw(" ".repeat(pad.saturating_sub(1))),
        Span::styled(right.to_string(), rs),
        Span::raw(" "),
        Span::styled("│", dim),
    ])
}

fn lines(provider: &str, model: &str, term_width: u16) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let dir_display = format_short_dir(&cwd);

    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(Color::Gray);

    let inner = term_width as usize;
    let content_w = inner.saturating_sub(3);

    let border = |open: &str, close: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("{open}{}{close}", "─".repeat(inner.saturating_sub(2))),
            dim,
        ))
    };

    vec![
        border("╭", "╮"),
        padded_row("bone", &format!("v{version}"), content_w, bold, muted),
        padded_row(
            &format!("{provider} · {model}"),
            &dir_display,
            content_w,
            dim,
            muted,
        ),
        border("╰", "╯"),
        Line::from(""),
    ]
}

/// Shorten a directory path to `first/.../last` for the banner display.
fn format_short_dir(path: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = path.iter().collect();
    if components.len() > 2 {
        let first = components[0].to_string_lossy();
        let last = components.last().unwrap().to_string_lossy();
        let sep = if first.ends_with('/') || first.ends_with('\\') {
            ""
        } else {
            "/"
        };
        format!("{first}{sep}.../{last}")
    } else {
        path.display().to_string()
    }
}
