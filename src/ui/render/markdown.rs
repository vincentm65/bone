//! Markdown rendering for the transcript via pulldown-cmark and syntect.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::borrow::Cow;
use std::io::Cursor;
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SyColor, FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Shared syntax set and theme, built once.
static PS: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let theme = include_str!("themes/dark_plus.tmTheme");
    ThemeSet::load_from_reader(&mut Cursor::new(theme)).expect("embedded Dark+ theme should parse")
});

const MUTED: Color = Color::DarkGray;
const INLINE_CODE: Color = Color::Gray;
const CODE_FALLBACK: Color = Color::Gray;
const RULE: Color = Color::DarkGray;
const TABLE_BORDER: Color = Color::DarkGray;
const TABLE_HEADER_FG: Color = Color::Rgb(140, 220, 220);

#[derive(Clone, Copy, Debug, Default)]
struct InlineStyle {
    heading: Option<HeadingLevel>,
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
    link: bool,
}

#[derive(Default)]
struct TableCell {
    spans: Vec<Span<'static>>,
}

#[derive(Default)]
struct TableRow {
    cells: Vec<TableCell>,
    is_header: bool,
}

#[derive(Default)]
struct MarkdownRenderer {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style: InlineStyle,
    width: usize,
    list_stack: Vec<Option<u64>>,
    quote_depth: usize,
    in_code: bool,
    code_lang: String,
    code_lines: Vec<String>,
    table_rows: Vec<TableRow>,
    table_alignments: Vec<Alignment>,
    active_link: Option<(String, bool)>,
}

impl MarkdownRenderer {
    fn new(width: u16) -> Self {
        Self {
            width: width.max(1) as usize,
            ..Self::default()
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.active_link.as_ref().is_some_and(|(_, local)| *local) {
            return;
        }
        self.push_styled_text(text);
    }

    fn push_styled_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.current
            .push(Span::styled(text.to_string(), self.current_style()));
    }

    fn push_prefix(&mut self, prefix: impl Into<String>) {
        self.current
            .push(Span::styled(prefix.into(), Style::default().fg(MUTED)));
    }

    fn current_style(&self) -> Style {
        let mut style = Style::default();
        if let Some(level) = self.style.heading {
            style = style.fg(Color::White).add_modifier(Modifier::BOLD);
            if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
        }
        if self.style.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.style.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.style.code {
            style = style.fg(INLINE_CODE);
        }
        if self.style.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.style.link {
            style = style.fg(Color::Gray);
        }
        style
    }

    fn begin_block(&mut self) {
        if self.current.is_empty() {
            for _ in 0..self.quote_depth {
                self.push_prefix("> ");
            }
        }
    }

    fn finish_line(&mut self) {
        if !self.current.is_empty() {
            self.lines.extend(wrap_prefixed_line(
                std::mem::take(&mut self.current),
                self.width,
            ));
        }
    }

    fn blank_line(&mut self) {
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            if self.quote_depth > 0 {
                let prefix = "> ".repeat(self.quote_depth);
                self.lines
                    .push(Line::styled(prefix, Style::default().fg(MUTED)));
            } else {
                self.lines.push(Line::raw(""));
            }
        }
    }

    fn begin_heading(&mut self, level: HeadingLevel) {
        self.finish_line();
        self.style.heading = Some(level);
        self.style.bold = true;
    }

    fn finish_heading(&mut self) {
        self.style.heading = None;
        self.style.bold = false;
        self.finish_line();
        self.blank_line();
    }

    fn begin_list_item(&mut self) {
        self.finish_line();
        self.begin_block();
        let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
        let marker = match self.list_stack.last_mut().and_then(Option::as_mut) {
            Some(next) => {
                let marker = format!("{next}. ");
                *next += 1;
                marker
            }
            None => "- ".to_string(),
        };
        self.push_prefix(format!("{indent}{marker}"));
    }

    // -- Table buffering (two-pass) --

    fn finish_table_cell(&mut self) {
        if let Some(row) = self.table_rows.last_mut() {
            row.cells.push(TableCell {
                spans: std::mem::take(&mut self.current),
            });
        }
    }

    fn finish_table_row(&mut self, is_header: bool) {
        if is_header && let Some(row) = self.table_rows.last_mut() {
            row.is_header = true;
        }
    }

    /// Compute column widths and emit aligned table lines.
    fn flush_table(&mut self) {
        let rows = std::mem::take(&mut self.table_rows);
        let alignments = std::mem::take(&mut self.table_alignments);
        if rows.is_empty() {
            return;
        }

        let num_cols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return;
        }

        // Compute max unicode width per column.
        let mut col_widths = vec![0usize; num_cols];
        for row in &rows {
            for (i, cell) in row.cells.iter().enumerate() {
                let w = table_cell_width(cell);
                col_widths[i] = col_widths[i].max(w);
            }
        }

        fit_table_width(&mut col_widths, self.width);
        let has_header = rows.first().is_some_and(|r| r.is_header);
        let boxed = table_total_width(&col_widths) <= self.width;

        if has_header && boxed {
            self.lines.push(table_border(&col_widths, '┌', '┬', '┐'));
        }

        for (row_idx, row) in rows.iter().enumerate() {
            self.lines.push(if boxed {
                table_row(row, &col_widths, &alignments)
            } else {
                table_pipe_row(row, self.width)
            });
            if boxed && row.is_header && row_idx == 0 {
                self.lines.push(table_border(&col_widths, '├', '┼', '┤'));
            }
        }

        if boxed {
            self.lines.push(table_border(&col_widths, '└', '┴', '┘'));
        }
        self.blank_line();
    }

    fn begin_code_block(&mut self, kind: CodeBlockKind<'_>) {
        self.finish_line();
        self.in_code = true;
        self.code_lang = match kind {
            CodeBlockKind::Fenced(lang) => lang.split_whitespace().next().unwrap_or("").to_string(),
            CodeBlockKind::Indented => String::new(),
        };
        self.code_lines.clear();
    }

    fn push_code_text(&mut self, text: &str) {
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').unwrap_or(line);
            self.code_lines.push(line.to_string());
        }
    }

    fn finish_code_block(&mut self) {
        let syntax = PS
            .find_syntax_by_token(&self.code_lang)
            .or_else(|| PS.find_syntax_by_extension(&self.code_lang))
            .unwrap_or_else(|| PS.find_syntax_plain_text());
        let code_lines = std::mem::take(&mut self.code_lines);
        let mut highlighter = HighlightLines::new(syntax, &THEME);
        for line in code_lines {
            let highlighted = highlight_line(&line, &mut highlighter);
            let mut spans = vec![Span::raw("  ")];
            spans.extend(highlighted.spans);
            self.lines.push(Line::from(spans));
        }
        self.blank_line();
        self.in_code = false;
        self.code_lang.clear();
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.finish_line();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }
}

fn sy_fg(c: SyColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

fn table_border(col_widths: &[usize], left: char, mid: char, right: char) -> Line<'static> {
    let style = Style::default().fg(TABLE_BORDER);
    let mut out = format!("{left}─");
    for (i, &w) in col_widths.iter().enumerate() {
        if i > 0 {
            out.push_str(&format!("─{mid}─"));
        }
        out.push_str(&"─".repeat(w.max(1)));
    }
    out.push_str(&format!("─{right}"));
    Line::styled(out, style)
}

fn table_cell_width(cell: &TableCell) -> usize {
    line_width(&cell.spans)
}

fn table_total_width(col_widths: &[usize]) -> usize {
    col_widths.iter().sum::<usize>() + col_widths.len() * 3 + 1
}

fn fit_table_width(col_widths: &mut [usize], width: usize) {
    while table_total_width(col_widths) > width {
        let Some((idx, _)) = col_widths
            .iter()
            .enumerate()
            .filter(|(_, current)| **current > 1)
            .max_by_key(|(_, current)| **current)
        else {
            break;
        };
        col_widths[idx] -= 1;
    }
}

fn truncate_spans(spans: &[Span<'static>], width: usize) -> Vec<Span<'static>> {
    if line_width(spans) <= width {
        return spans.to_vec();
    }
    if width == 0 {
        return Vec::new();
    }

    let text_width = width.saturating_sub(1);
    let mut remaining = text_width;
    let mut truncated = Vec::new();
    for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch_width > remaining {
                break;
            }
            text.push(ch);
            remaining = remaining.saturating_sub(ch_width);
        }
        if !text.is_empty() {
            truncated.push(Span::styled(text, span.style));
        }
        if remaining == 0 {
            break;
        }
    }
    truncated.push(Span::raw("…"));
    truncated
}

fn table_row(row: &TableRow, col_widths: &[usize], alignments: &[Alignment]) -> Line<'static> {
    let border_style = Style::default().fg(TABLE_BORDER);
    let text_style = if row.is_header {
        Style::default()
            .fg(TABLE_HEADER_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled("│ ".to_string(), border_style)];

    for (col_idx, cell) in row.cells.iter().enumerate() {
        if col_idx > 0 {
            spans.push(Span::styled(" │ ".to_string(), border_style));
        }
        spans.extend(aligned_cell(
            cell,
            col_widths.get(col_idx).copied().unwrap_or(0),
            alignments.get(col_idx).copied().unwrap_or(Alignment::None),
            text_style,
        ));
    }
    spans.push(Span::styled(" │".to_string(), border_style));
    Line::from(spans)
}

fn table_pipe_row(row: &TableRow, width: usize) -> Line<'static> {
    let style = if row.is_header {
        Style::default()
            .fg(TABLE_HEADER_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled("| ", Style::default().fg(TABLE_BORDER))];
    for (idx, cell) in row.cells.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(TABLE_BORDER)));
        }
        spans.extend(
            cell.spans
                .iter()
                .cloned()
                .map(|span| Span::styled(span.content.into_owned(), span.style.patch(style))),
        );
    }
    spans.push(Span::styled(" |", Style::default().fg(TABLE_BORDER)));
    Line::from(truncate_spans(&spans, width))
}

fn aligned_cell(
    cell: &TableCell,
    width: usize,
    align: Alignment,
    style: Style,
) -> Vec<Span<'static>> {
    let content = truncate_spans(&cell.spans, width);
    let content_width = line_width(&content);
    let pad = width.saturating_sub(content_width);
    let (left, right) = match align {
        Alignment::Center => (pad / 2, pad - pad / 2),
        Alignment::Right => (pad, 0),
        _ => (0, pad),
    };
    let mut spans = vec![Span::raw(" ".repeat(left))];
    spans.extend(
        content
            .into_iter()
            .map(|span| Span::styled(span.content.into_owned(), span.style.patch(style))),
    );
    spans.push(Span::raw(" ".repeat(right)));
    spans
}

fn wrap_prefixed_line(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let prefix_len = muted_prefix_len(&spans);
    let prefix_width = line_width(&spans[..prefix_len]);
    if prefix_width == 0 || line_width(&spans) <= width {
        return vec![Line::from(spans)];
    }

    wrap_words(
        spans[..prefix_len].to_vec(),
        words_from_spans(&spans[prefix_len..]),
        width,
    )
}

fn muted_prefix_len(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .take_while(|span| span.style.fg == Some(MUTED))
        .count()
}

fn line_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn words_from_spans(spans: &[Span<'static>]) -> Vec<Vec<Span<'static>>> {
    let mut words = Vec::new();
    let mut word = Vec::new();

    for span in spans {
        for ch in span.content.chars() {
            if ch.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push(Span::styled(ch.to_string(), span.style));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn wrap_words(
    prefix: Vec<Span<'static>>,
    words: Vec<Vec<Span<'static>>>,
    width: usize,
) -> Vec<Line<'static>> {
    // Block-quote prefixes should repeat on continuation lines.
    // List/other prefixes should use blank spaces to preserve alignment.
    let prefix_text: String = prefix.iter().map(|s| s.content.as_ref()).collect();
    let continuation = if prefix_text.contains("> ") {
        prefix.clone()
    } else {
        vec![Span::styled(
            " ".repeat(line_width(&prefix)),
            Style::default().fg(MUTED),
        )]
    };
    let mut lines = Vec::new();
    let mut current = prefix;
    let mut current_width = line_width(&current);
    let mut has_word = false;

    for word in words {
        let word_width = line_width(&word);
        let separator = usize::from(has_word);
        if has_word && current_width + separator + word_width > width {
            lines.push(Line::from(std::mem::take(&mut current)));
            current = continuation.clone();
            current_width = line_width(&current);
            has_word = false;
        }
        if has_word {
            current.push(Span::raw(" "));
            current_width += 1;
        }
        current.extend(word);
        current_width += word_width;
        has_word = true;
    }

    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn syntect_style(sy_style: syntect::highlighting::Style) -> Style {
    let fg = if sy_style.foreground.a == 0 {
        CODE_FALLBACK
    } else {
        sy_fg(sy_style.foreground)
    };
    let mut style = Style::default().fg(fg);
    let fs = sy_style.font_style;
    if fs.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if fs.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if fs.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// Highlight a single line of code using syntect -> ratatui spans.
fn highlight_line(line: &str, highlighter: &mut HighlightLines<'_>) -> Line<'static> {
    let ranges = highlighter.highlight_line(line, &PS).unwrap_or_default();
    if ranges.is_empty() {
        return Line::raw(line.to_string());
    }

    Line::from(
        ranges
            .into_iter()
            .map(|(style, text)| Span::styled(text.to_string(), syntect_style(style)))
            .collect::<Vec<_>>(),
    )
}

fn is_local_link(destination: &str) -> bool {
    !destination.starts_with("http://")
        && !destination.starts_with("https://")
        && !destination.starts_with("mailto:")
        && !destination.starts_with('#')
}

fn display_local_link(destination: &str) -> String {
    // Strip the file:// scheme if present.
    let target = destination.strip_prefix("file://").unwrap_or(destination);

    // Try to resolve the path relative to CWD using std::path::Path
    // so it works correctly on Windows (backslash separators, drive letters).
    let Ok(cwd) = std::env::current_dir() else {
        return target.to_string();
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let target_path = std::path::Path::new(target);

    // If the target is an absolute path under CWD, show it relative.
    if let Ok(relative) = target_path.strip_prefix(&cwd) {
        return relative.to_string_lossy().into_owned();
    }

    target.to_string()
}

fn has_table_fence(content: &str) -> bool {
    content.contains("```md\n")
        || content.contains("```markdown\n")
        || content.contains("```md\r\n")
        || content.contains("```markdown\r\n")
}

fn unwrap_markdown_table_fences(content: &str) -> Cow<'_, str> {
    if !has_table_fence(content) {
        return Cow::Borrowed(content);
    }

    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut output = String::with_capacity(content.len());
    let mut idx = 0;
    while idx < lines.len() {
        let opening = lines[idx].trim();
        if opening != "```md" && opening != "```markdown" {
            output.push_str(lines[idx]);
            idx += 1;
            continue;
        }

        let Some(close) = lines[idx + 1..]
            .iter()
            .position(|line| line.trim() == "```")
            .map(|relative| idx + 1 + relative)
        else {
            // Unclosed fence — emit the body as-is without the opening
            // marker so the parser doesn't treat it as a fenced code block.
            for line in &lines[idx + 1..] {
                output.push_str(line);
            }
            break;
        };
        let body = lines[idx + 1..close].concat();
        if contains_markdown_table(&body) {
            output.push_str(&body);
        } else {
            for line in &lines[idx..=close] {
                output.push_str(line);
            }
        }
        idx = close + 1;
    }
    Cow::Owned(output)
}

fn contains_markdown_table(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    lines.windows(2).any(|pair| {
        pair[0].contains('|')
            && {
                // Both lines must have the same number of cells (pipe count must match).
                let header_pipes = pair[0].matches('|').count();
                let sep_pipes = pair[1].matches('|').count();
                header_pipes > 1 && header_pipes == sep_pipes
            }
            && pair[1].trim_matches('|').split('|').all(|cell| {
                let cell = cell.trim().trim_matches(':');
                cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
            })
    })
}

/// Render markdown content into ratatui lines.
pub fn render_markdown(content: &str, width: u16) -> Vec<Line<'static>> {
    let normalized = unwrap_markdown_table_fences(content);
    let parser = Parser::new_ext(&normalized, markdown_options());
    let mut renderer = MarkdownRenderer::new(width);

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => renderer.begin_block(),
                Tag::Heading { level, .. } => {
                    renderer.begin_heading(level);
                }
                Tag::Emphasis => renderer.style.italic = true,
                Tag::Strong => renderer.style.bold = true,
                Tag::Link { dest_url, .. } => {
                    let dest = dest_url.to_string();
                    let local = is_local_link(&dest);
                    renderer.style.link = true;
                    if local {
                        renderer.push_styled_text(&display_local_link(&dest));
                    }
                    renderer.active_link = Some((dest, local));
                }
                Tag::Image { dest_url, .. } => {
                    renderer.begin_block();
                    renderer.push_text("[image");
                    if !dest_url.is_empty() {
                        renderer.push_text(": ");
                        renderer.push_text(&dest_url);
                    }
                    renderer.push_text("]");
                }
                Tag::BlockQuote(_) => renderer.quote_depth += 1,
                Tag::List(start) => renderer.list_stack.push(start),
                Tag::Item => renderer.begin_list_item(),
                Tag::CodeBlock(kind) => renderer.begin_code_block(kind),
                Tag::Table(alignments) => {
                    renderer.finish_line();
                    renderer.table_alignments = alignments;
                }
                Tag::TableHead | Tag::TableRow => {
                    renderer.table_rows.push(TableRow::default());
                }
                Tag::FootnoteDefinition(name) => {
                    renderer.finish_line();
                    renderer.push_prefix(format!("[^{name}]: "));
                }
                Tag::Strikethrough => renderer.style.strikethrough = true,
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    renderer.finish_line();
                    renderer.blank_line();
                }
                TagEnd::Heading(_) => renderer.finish_heading(),
                TagEnd::Emphasis => renderer.style.italic = false,
                TagEnd::Strong => renderer.style.bold = false,
                TagEnd::Link => {
                    if let Some((dest, local)) = renderer.active_link.take()
                        && !local
                    {
                        renderer.push_styled_text(&format!(" - {dest}"));
                    }
                    renderer.style.link = false;
                }
                TagEnd::BlockQuote(_) => {
                    renderer.finish_line();
                    renderer.quote_depth = renderer.quote_depth.saturating_sub(1);
                    // Remove the trailing quote-marker line left by End(Paragraph).
                    // The blank_line() call in End(Paragraph) adds a marker-only
                    // line like "> " - strip it when the block quote closes.
                    if renderer.lines.last().is_some_and(|line| {
                        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                        let trimmed = text.trim_end();
                        !trimmed.is_empty() && trimmed.split_whitespace().all(|w| w == ">")
                    }) {
                        renderer.lines.pop();
                    }
                    renderer.blank_line();
                }
                TagEnd::List(_) => {
                    renderer.list_stack.pop();
                    renderer.blank_line();
                }
                TagEnd::Item => renderer.finish_line(),
                TagEnd::CodeBlock => renderer.finish_code_block(),
                TagEnd::Table => renderer.flush_table(),
                TagEnd::TableHead => renderer.finish_table_row(true),
                TagEnd::TableRow => renderer.finish_table_row(false),
                TagEnd::TableCell => renderer.finish_table_cell(),
                TagEnd::FootnoteDefinition => {
                    renderer.finish_line();
                    renderer.blank_line();
                }
                TagEnd::Strikethrough => renderer.style.strikethrough = false,
                _ => {}
            },
            Event::Text(text) => {
                if renderer.in_code {
                    renderer.push_code_text(&text);
                } else {
                    renderer.push_text(&text);
                }
            }
            Event::Code(text) => {
                renderer.style.code = true;
                renderer.push_text(&text);
                renderer.style.code = false;
            }
            Event::SoftBreak => {
                if renderer.in_code {
                    renderer.push_code_text("\n");
                } else {
                    renderer.push_text(" ");
                }
            }
            Event::HardBreak => {
                if renderer.in_code {
                    renderer.push_code_text("\n");
                } else {
                    renderer.finish_line();
                    renderer.begin_block();
                }
            }
            Event::Rule => {
                renderer.blank_line();
                renderer.finish_line();
                let w = width.max(1) as usize;
                renderer
                    .lines
                    .push(Line::styled("─".repeat(w), Style::default().fg(RULE)));
                renderer.blank_line();
            }
            Event::TaskListMarker(checked) => {
                renderer.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(name) => renderer.push_text(&format!("[^{name}]")),
            Event::Html(html) | Event::InlineHtml(html) => renderer.push_text(&html),
            _ => {}
        }
    }

    renderer.finish()
}
