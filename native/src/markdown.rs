//! Minimal Markdown parsing and egui rendering for transcript rows.
//!
//! Parsing is kept pure ([`parse_markdown`]) and separated from rendering
//! ([`render_blocks`]) so the parse result can be cached in the UI layer while
//! the renderer stays a thin egui projection. Only a deliberate subset of
//! CommonMark is rendered: headings, paragraphs, inline emphasis/strike/code,
//! fenced/indented code blocks, ordered & unordered lists, block quotes and
//! horizontal rules. Safe browser URLs open normally (see [`is_safe_url`]);
//! recognized local file references require an explicit editor confirmation.

use crate::theme::ThemeColors;
use eframe::egui::{
    self, Align, AsIdSalt, FontSelection, RichText, Style, TextFormat, TextStyle, Ui,
    text::LayoutJob,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SyColor, FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Shared syntax set, built once. The newlines variant closes line-scoped
/// contexts (e.g. `#` comments) on `\n`, so highlighting feeds each line with
/// its terminator.
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Bundled syntect themes. The desktop app picks one by background luminance;
/// building a per-theme syntect theme each frame would re-parse scope
/// selectors, so a static set is preferred.
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// One contiguous run of text sharing the same inline styling.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// Safe browser URL or a parsed local file reference.
    pub url: Option<String>,
}

/// A single rendered transcript block.
#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    Paragraph {
        runs: Vec<Run>,
        /// Bullet/number marker when the paragraph is a list item.
        marker: Option<String>,
        /// Total indent levels (block quotes + enclosing list nesting).
        depth: usize,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    Table {
        headers: Vec<Vec<Run>>,
        /// Per-column horizontal alignment (from the table's delimiter row).
        alignments: Vec<Align>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
}

/// Buffered state while a table is being parsed. pulldown-cmark emits the
/// header row as `TableHead`/`TableCell` events and body rows as
/// `TableRow`/`TableCell`, so cells are collected here and assembled on
/// `TagEnd::Table`.
#[derive(Default)]
struct TableBuild {
    alignments: Vec<Align>,
    header: Vec<Vec<Run>>,
    rows: Vec<Vec<Vec<Run>>>,
    current_row: Vec<Vec<Run>>,
    in_header: bool,
}

/// A URL is "safe" only if it uses a scheme we are willing to hand to the
/// system browser. Everything else is rendered as plain, non-clickable text.
pub fn is_safe_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

fn push_run(
    runs: &mut Vec<Run>,
    text: &str,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    url: Option<String>,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.bold == bold
        && last.italic == italic
        && last.strike == strike
        && last.code == code
        && last.url == url
    {
        last.text.push_str(text);
        return;
    }
    runs.push(Run {
        text: text.to_string(),
        bold,
        italic,
        strike,
        code,
        url,
    });
}

/// Parse a Markdown string into renderable blocks. Pure and allocation-only.
pub fn parse_markdown(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut heading_level: Option<u8> = None;
    let mut code: Option<(Option<String>, String)> = None;
    let mut list_stack: Vec<(bool, u64)> = Vec::new(); // (ordered, next number)
    let mut quote_depth: usize = 0;
    let mut current_marker: Option<String> = None;
    let (mut bold, mut italic, mut strike) = (0usize, 0usize, 0usize);
    let mut url: Option<String> = None;
    let mut table: Option<TableBuild> = None;
    let mut in_footnote = false;

    let list_depth = |list_stack: &[(bool, u64)]| {
        if list_stack.is_empty() {
            0
        } else {
            list_stack.len()
        }
    };

    for event in Parser::new_ext(text, markdown_options()) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    heading_level = Some(heading_size(level));
                    runs.clear();
                }
                Tag::Paragraph => runs.clear(),
                Tag::CodeBlock(kind) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(info) if !info.is_empty() => Some(info.to_string()),
                        _ => None,
                    };
                    code = Some((language, String::new()));
                }
                Tag::BlockQuote(_) => quote_depth += 1,
                Tag::List(number) => list_stack.push((number.is_some(), number.unwrap_or(1))),
                Tag::Item => {
                    if let Some(top) = list_stack.last_mut() {
                        let marker = if top.0 {
                            let n = top.1;
                            top.1 = n + 1;
                            format!("{n}.")
                        } else {
                            "•".to_string()
                        };
                        current_marker = Some(marker);
                    }
                }
                Tag::Strong => bold += 1,
                Tag::Emphasis => italic += 1,
                Tag::Strikethrough => strike += 1,
                Tag::Link { dest_url, .. } => {
                    url = Some(dest_url.to_string())
                        .filter(|u| is_safe_url(u) || crate::file_refs::parse(u).is_some());
                }
                Tag::Table(alignments) => {
                    table = Some(TableBuild {
                        alignments: alignments.iter().map(map_alignment).collect(),
                        ..TableBuild::default()
                    });
                    runs.clear();
                }
                Tag::TableHead => {
                    if let Some(table) = table.as_mut() {
                        table.in_header = true;
                        table.current_row.clear();
                    }
                    runs.clear();
                }
                Tag::TableRow => {
                    if let Some(table) = table.as_mut() {
                        table.in_header = false;
                        table.current_row.clear();
                    }
                    runs.clear();
                }
                Tag::TableCell => runs.clear(),
                Tag::FootnoteDefinition(name) => {
                    in_footnote = true;
                    current_marker = Some(format!("[^{name}]: "));
                    runs.clear();
                }
                Tag::Image { .. } => {} // alt text arrives as Text; no image rendering yet
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    if let Some(level) = heading_level.take() {
                        blocks.push(Block::Heading {
                            level,
                            runs: std::mem::take(&mut runs),
                        });
                    } else {
                        runs.clear();
                    }
                }
                TagEnd::Paragraph => {
                    if !runs.is_empty() {
                        let marker = current_marker.clone();
                        blocks.push(Block::Paragraph {
                            runs: std::mem::take(&mut runs),
                            marker,
                            depth: quote_depth + list_depth(&list_stack),
                        });
                    }
                    // A footnote prefix applies only to its first paragraph.
                    if in_footnote {
                        current_marker = None;
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((language, body)) = code.take() {
                        blocks.push(Block::Code {
                            language,
                            text: body,
                        });
                    }
                }
                TagEnd::TableCell => {
                    if let Some(table) = table.as_mut() {
                        table.current_row.push(std::mem::take(&mut runs));
                    }
                }
                TagEnd::TableHead => {
                    if let Some(table) = table.as_mut() {
                        table.header = std::mem::take(&mut table.current_row);
                        table.in_header = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(table) = table.as_mut() {
                        table.rows.push(std::mem::take(&mut table.current_row));
                    }
                }
                TagEnd::Table => {
                    if let Some(table) = table.take() {
                        blocks.push(Block::Table {
                            headers: table.header,
                            alignments: table.alignments,
                            rows: table.rows,
                        });
                    }
                }
                TagEnd::FootnoteDefinition => {
                    in_footnote = false;
                    current_marker = None;
                }
                TagEnd::BlockQuote(_) => quote_depth = quote_depth.saturating_sub(1),
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::Item => {
                    // Tight list items are not wrapped in a paragraph, so their
                    // text is only flushed when the item closes.
                    if !runs.is_empty() {
                        let marker = current_marker.clone();
                        blocks.push(Block::Paragraph {
                            runs: std::mem::take(&mut runs),
                            marker,
                            depth: quote_depth + list_depth(&list_stack),
                        });
                    }
                    current_marker = None;
                }
                TagEnd::Strong => bold = bold.saturating_sub(1),
                TagEnd::Emphasis => italic = italic.saturating_sub(1),
                TagEnd::Strikethrough => strike = strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => url = None,
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, body)) = code.as_mut() {
                    body.push_str(&t);
                } else {
                    push_run(
                        &mut runs,
                        &t,
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        url.clone(),
                    );
                }
            }
            Event::Code(c) if code.is_none() => {
                let reference = crate::file_refs::parse(&c).map(|_| c.to_string());
                push_run(&mut runs, &c, false, false, false, true, reference);
            }
            Event::SoftBreak if code.is_none() => {
                push_run(
                    &mut runs,
                    " ",
                    bold > 0,
                    italic > 0,
                    strike > 0,
                    false,
                    url.clone(),
                );
            }
            Event::HardBreak if code.is_none() => {
                push_run(
                    &mut runs,
                    "\n",
                    bold > 0,
                    italic > 0,
                    strike > 0,
                    false,
                    url.clone(),
                );
            }
            Event::Rule => blocks.push(Block::Rule),
            Event::TaskListMarker(checked) => {
                let checkbox = if checked { "[x] " } else { "[ ] " };
                current_marker = Some(match current_marker.take() {
                    Some(marker) => format!("{marker} {checkbox}"),
                    None => checkbox.to_string(),
                });
            }
            Event::FootnoteReference(name) => {
                push_run(
                    &mut runs,
                    &format!("[^{name}]"),
                    bold > 0,
                    italic > 0,
                    strike > 0,
                    false,
                    None,
                );
            }
            _ => {}
        }
    }

    if let Some((language, body)) = code {
        blocks.push(Block::Code {
            language,
            text: body,
        });
    }

    blocks
}

fn heading_size(level: HeadingLevel) -> u8 {
    level as u8
}

/// Enabled CommonMark extensions, mirroring the TUI renderer.
fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// Map a table column alignment to the egui horizontal alignment used to lay
/// out its cells.
fn map_alignment(alignment: &pulldown_cmark::Alignment) -> Align {
    match alignment {
        pulldown_cmark::Alignment::Center => Align::Center,
        pulldown_cmark::Alignment::Right => Align::Max,
        _ => Align::Min,
    }
}

/// Build an egui [`LayoutJob`] from a run of styled text. `base` colors runs
/// that carry no role-specific color (used for headings); inline code and link
/// runs always take their Markdown role colors. `strong` forces the whole job
/// bold (headings and table headers). Inline code runs get a subtle background,
/// and when `line_height` is set every section inherits it (prose breathing).
fn build_job(
    runs: &[Run],
    style: &Style,
    size: Option<f32>,
    base: Option<egui::Color32>,
    strong: bool,
    line_height: Option<f32>,
    colors: &ThemeColors,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let code_bg = style.visuals.faint_bg_color;
    for run in runs {
        let mut rich = RichText::new(run.text.clone());
        if let Some(size) = size {
            rich = rich.size(size);
        }
        if strong || run.bold {
            rich = rich
                .strong()
                .family(egui::TextStyle::Heading.resolve(style).family);
        }
        if run.italic {
            rich = rich.italics();
        }
        if run.strike {
            rich = rich.strikethrough();
        }
        if run.code {
            rich = rich.monospace();
        }
        let color = if run.code {
            Some(colors.markdown_inline_code)
        } else if run.url.is_some() {
            Some(colors.markdown_link)
        } else {
            base
        };
        if let Some(color) = color {
            rich = rich.color(color);
        }
        let start = job.sections.len();
        rich.append_to(&mut job, style, FontSelection::Default, Align::TOP);
        for section in &mut job.sections[start..] {
            if run.code {
                section.format.background = code_bg;
            }
            if let Some(line_height) = line_height {
                section.format.line_height = Some(line_height);
            }
        }
    }
    job
}

/// Intrinsic width for a short, ordinary prompt. Measure text only (never render
/// widgets), so links and complex blocks cannot trigger duplicate UI side effects.
/// Long/structured messages use the bounded conversation bubble instead.
pub(crate) fn short_paragraph_width(
    ui: &Ui,
    blocks: &[Block],
    colors: &ThemeColors,
) -> Option<f32> {
    let [
        Block::Paragraph {
            runs,
            marker: None,
            depth: 0,
        },
    ] = blocks
    else {
        return None;
    };
    if runs.iter().any(|run| run.url.is_some())
        || runs.iter().map(|run| run.text.len()).sum::<usize>() > 512
    {
        return None;
    }
    let job = build_job(
        runs,
        ui.style(),
        None,
        None,
        false,
        Some(PROSE_LINE_HEIGHT),
        colors,
    );
    Some(ui.fonts_mut(|fonts| fonts.layout_job(job).size().x).ceil())
}

/// Render a list of parsed blocks into the transcript column.
pub fn render_blocks(
    ui: &mut Ui,
    salt: impl AsIdSalt + Copy,
    blocks: &[Block],
    colors: &ThemeColors,
) {
    for (index, block) in blocks.iter().enumerate() {
        match block {
            Block::Rule => {
                ui.add_space(4.0);
                ui.scope(|ui| {
                    ui.visuals_mut().widgets.noninteractive.bg_stroke.color =
                        soften(colors.markdown_rule, 0.16);
                    ui.separator();
                });
                ui.add_space(4.0);
            }
            Block::Code { language, text } => {
                render_code_block(ui, language.as_deref(), text, colors)
            }
            Block::Table {
                headers,
                alignments,
                rows,
            } => render_table(ui, headers, alignments, rows, colors),
            Block::Heading { level, runs } => {
                ui.add_space(if index == 0 { 2.0 } else { 12.0 });
                let size = heading_pixel_size(*level);
                let job = build_job(
                    runs,
                    ui.style(),
                    Some(size),
                    Some(colors.markdown_heading),
                    true,
                    None,
                    colors,
                );
                ui.add(egui::Label::new(job).selectable(true));
                ui.add_space(2.0);
            }
            Block::Paragraph {
                runs,
                marker,
                depth,
            } => {
                if *depth > 0 {
                    ui.scope(|ui| {
                        // Lists need indentation, not a quote-like vertical rule.
                        if marker.is_some() {
                            ui.visuals_mut().indent_has_left_vline = false;
                        }
                        ui.indent((salt, index), |ui| {
                            render_inline(ui, runs, marker.as_deref(), colors)
                        });
                    });
                } else {
                    render_inline(ui, runs, marker.as_deref(), colors);
                }
                if index + 1 < blocks.len() {
                    ui.add_space(if marker.is_some() { 2.0 } else { 6.0 });
                }
            }
        }
    }
}

fn heading_pixel_size(level: u8) -> f32 {
    match level {
        1 => 24.0,
        2 => 21.0,
        3 => 18.0,
        4 => 16.0,
        _ => 15.0,
    }
}

/// A readable 1.5x line height for the 16pt conversation face.
const PROSE_LINE_HEIGHT: f32 = 24.0;

/// Reduce a color's effective coverage toward transparency by a factor in
/// `0..=1`. egui colors are premultiplied, so scaling the whole color softens a
/// border or rule without altering its hue.
fn soften(color: egui::Color32, factor: f32) -> egui::Color32 {
    color.linear_multiply(factor.clamp(0.0, 1.0))
}

/// Render a parsed Markdown table as a bordered egui grid. Rows are padded to
/// the widest column count so the grid stays aligned, and the delimiter row's
/// per-column alignment is honored within each cell.
fn render_table(
    ui: &mut Ui,
    headers: &[Vec<Run>],
    alignments: &[Align],
    rows: &[Vec<Vec<Run>>],
    colors: &ThemeColors,
) {
    let cols = rows
        .iter()
        .map(Vec::len)
        .chain(std::iter::once(headers.len()))
        .max()
        .unwrap_or(0);
    if cols == 0 {
        return;
    }
    ui.add_space(4.0);
    egui::Frame::default()
        .stroke(egui::Stroke::new(
            1.0,
            soften(colors.markdown_table_border, 0.16),
        ))
        .inner_margin(8.0)
        .corner_radius(6.0)
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(ui.next_auto_id())
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new(ui.next_auto_id())
                        .num_columns(cols)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            if !headers.is_empty() {
                                for col in 0..cols {
                                    let cell = headers.get(col).map(Vec::as_slice).unwrap_or(&[]);
                                    table_cell(
                                        ui,
                                        cell,
                                        column_align(alignments, col),
                                        true,
                                        colors,
                                    );
                                }
                                ui.end_row();
                            }
                            for row in rows {
                                for col in 0..cols {
                                    let cell = row.get(col).map(Vec::as_slice).unwrap_or(&[]);
                                    table_cell(
                                        ui,
                                        cell,
                                        column_align(alignments, col),
                                        false,
                                        colors,
                                    );
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    ui.add_space(4.0);
}

fn column_align(alignments: &[Align], col: usize) -> Align {
    alignments.get(col).copied().unwrap_or(Align::Min)
}

/// One table cell: a label laid out with the column's horizontal alignment
/// inside the grid's column width.
fn table_cell(ui: &mut Ui, runs: &[Run], align: Align, header: bool, colors: &ThemeColors) {
    let base = header.then_some(colors.markdown_table_header);
    let job = build_job(
        runs,
        ui.style(),
        Some(14.0),
        base,
        header,
        Some(22.0),
        colors,
    );
    ui.with_layout(egui::Layout::top_down(align), |ui| {
        ui.add(
            egui::Label::new(job)
                .selectable(true)
                .wrap_mode(egui::TextWrapMode::Extend),
        );
    });
}

/// Render the inline content of a paragraph. When no run carries a link the
/// whole paragraph is one selectable, wrapping label (the fast path). Only
/// when a link is present do we fall back to a wrapping row of segments so each
/// safe link becomes an independent, clickable widget.
fn render_inline(ui: &mut Ui, runs: &[Run], marker: Option<&str>, colors: &ThemeColors) {
    if !runs.iter().any(|run| run.url.is_some()) {
        let style = ui.style();
        let code_bg = style.visuals.faint_bg_color;
        let mut job = LayoutJob::default();
        if let Some(marker) = marker {
            let marker = RichText::new(marker.to_string()).color(colors.markdown_marker);
            marker.append_to(&mut job, style, FontSelection::Default, Align::TOP);
            let gap = RichText::new("  ").color(colors.markdown_marker);
            gap.append_to(&mut job, style, FontSelection::Default, Align::TOP);
        }
        for run in runs {
            let mut rich = RichText::new(run.text.clone());
            if run.bold {
                rich = rich
                    .strong()
                    .family(egui::TextStyle::Heading.resolve(style).family);
            }
            if run.italic {
                rich = rich.italics();
            }
            if run.strike {
                rich = rich.strikethrough();
            }
            if run.code {
                rich = rich.monospace().color(colors.markdown_inline_code);
            }
            let start = job.sections.len();
            rich.append_to(&mut job, style, FontSelection::Default, Align::TOP);
            for section in &mut job.sections[start..] {
                section.format.line_height = Some(PROSE_LINE_HEIGHT);
                if run.code {
                    section.format.background = code_bg;
                }
            }
        }
        ui.add(egui::Label::new(job).selectable(true));
        return;
    }

    ui.horizontal_wrapped(|ui| {
        if let Some(marker) = marker {
            ui.colored_label(colors.markdown_marker, marker);
            ui.label("  ");
        }
        let mut i = 0;
        while i < runs.len() {
            let url = runs[i].url.clone();
            let mut segment = Vec::new();
            while i < runs.len() && runs[i].url == url {
                segment.push(runs[i].clone());
                i += 1;
            }
            let job = build_job(
                &segment,
                ui.style(),
                None,
                None,
                false,
                Some(PROSE_LINE_HEIGHT),
                colors,
            );
            match url {
                None => {
                    ui.add(egui::Label::new(job));
                }
                Some(url) if is_safe_url(&url) => {
                    ui.hyperlink_to(job, url);
                }
                Some(reference) => {
                    if ui
                        .link(job)
                        .on_hover_text("Open file in a local editor…")
                        .clicked()
                    {
                        crate::file_refs::request(ui.ctx(), &reference);
                    }
                }
            }
        }
    });
}

/// Cap on the code lines laid out per block. A single unbounded label for a
/// huge block would make the row (and the frame) unbounded; the Copy button
/// always copies the full text, so nothing is lost.
const MAX_CODE_LINES: usize = 200;

fn render_code_block(ui: &mut Ui, language: Option<&str>, text: &str, colors: &ThemeColors) {
    ui.add_space(6.0);
    let (shown, remaining) = preformatted_prefix(text, MAX_CODE_LINES);
    if !shown.is_empty() {
        let visuals = ui.visuals();
        // One compact container holds both the slim header and the highlighted
        // body so the block reads as a single, integrated surface.
        let frame = egui::Frame::default()
            .fill(visuals.code_bg_color)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .corner_radius(6.0);
        frame.show(ui, |ui| {
            // Slim header: the language tag on the left, Copy pushed to the right.
            ui.horizontal(|ui| {
                ui.set_height(18.0);
                if let Some(language) = language {
                    ui.label(RichText::new(language.to_string()).small().weak());
                } else {
                    ui.add_space(2.0);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let copy = egui::Button::new(RichText::new("Copy").small().weak()).frame(false);
                    if ui.add(copy).clicked() {
                        ui.ctx().copy_text(text.to_string());
                    }
                });
            });
            ui.add_space(6.0);
            let job = code_layout_job(ui, language, &shown, colors);
            egui::ScrollArea::horizontal()
                .id_salt(ui.next_auto_id())
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(job)
                            .selectable(true)
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
            if remaining > 0 {
                ui.label(
                    RichText::new(format!(
                        "… {remaining} more lines — Copy for the full block"
                    ))
                    .small()
                    .weak(),
                );
            }
        });
    }
    ui.add_space(6.0);
}

/// Build a syntect-highlighted monospace [`LayoutJob`] for a fenced code block.
/// Each line is highlighted with its `\n` so line-scoped scopes close, then the
/// terminator is re-emitted as an unstyled break.
fn code_layout_job(ui: &Ui, language: Option<&str>, text: &str, colors: &ThemeColors) -> LayoutJob {
    let syntax = language
        .and_then(|lang| {
            SYNTAX_SET
                .find_syntax_by_token(lang)
                .or_else(|| SYNTAX_SET.find_syntax_by_extension(lang))
        })
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let theme = code_theme(colors);
    let font_id = TextStyle::Monospace.resolve(ui.style().as_ref());
    let default_fg = theme
        .settings
        .foreground
        .map(sy_color)
        .unwrap_or(egui::Color32::from_rgb(0xd4, 0xd4, 0xd4));
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut job = LayoutJob::default();
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line_with_nl = format!("{line}\n");
        let ranges = highlighter
            .highlight_line(&line_with_nl, &SYNTAX_SET)
            .unwrap_or_default();
        for (style, segment) in ranges {
            let segment = segment.strip_suffix('\n').unwrap_or(segment);
            if segment.is_empty() {
                continue;
            }
            let color = if style.foreground.a == 0 {
                default_fg
            } else {
                sy_color(style.foreground)
            };
            job.append(
                segment,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color,
                    italics: style.font_style.contains(FontStyle::ITALIC),
                    ..Default::default()
                },
            );
        }
        if raw.ends_with('\n') {
            job.append(
                "\n",
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    ..Default::default()
                },
            );
        }
    }
    job
}

/// The bundled syntect theme for the current background: a dark theme on dark
/// backgrounds, a light one otherwise.
fn code_theme(colors: &ThemeColors) -> &'static syntect::highlighting::Theme {
    if colors.syntax_dark {
        &THEME_SET.themes["base16-ocean.dark"]
    } else {
        &THEME_SET.themes["InspiredGitHub"]
    }
}

fn sy_color(color: SyColor) -> egui::Color32 {
    egui::Color32::from_rgb(color.r, color.g, color.b)
}

/// Split `text` into the first `max_lines` lines — newlines preserved exactly,
/// no truncation within a line — plus the number of lines beyond the bound.
/// Pure, so the bounded rendering can be tested without a UI.
pub fn preformatted_prefix(text: &str, max_lines: usize) -> (String, usize) {
    let mut shown = String::new();
    let mut count = 0;
    for line in text.split_inclusive('\n') {
        if count >= max_lines {
            break;
        }
        shown.push_str(line);
        count += 1;
    }
    let total = text.lines().count();
    // Drop the newline of a truncated last line so the label does not gain a
    // phantom empty line; when the whole text fits, it stays byte-for-byte.
    if count < total && shown.ends_with('\n') {
        shown.pop();
    }
    (shown, total.saturating_sub(count))
}

/// Marker character of a numbered diff line, mirroring the TUI's
/// `numbered_diff_parts`: columns 0..5 hold a right-aligned line number and
/// column 6 is the ` `/`+`/`-` marker. Returns `None` for lines that are not
/// numbered diff rows (headers, `@@` hunk lines, context prose).
fn numbered_diff_marker(line: &str) -> Option<char> {
    let gutter = line.get(..8)?;
    let marker = *gutter.as_bytes().get(6)? as char;
    let has_number = gutter.get(..5)?.trim().parse::<usize>().is_ok();
    (has_number && matches!(marker, ' ' | '+' | '-')).then_some(marker)
}

/// A muted, readable foreground tint derived from a diff band fill: lighten a
/// dark fill or darken a light one, so `+`/`-` bodies read as green/red over
/// their band without shouting.
fn diff_line_fg(fill: egui::Color32) -> egui::Color32 {
    let luma = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    let target = if luma < 128.0 {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };
    crate::theme::mix(fill, target, 0.5)
}

/// Render a daemon-produced diff preview — a system row whose content begins
/// with `\n`. Numbered `+`/`-` lines get a background band padded to the widest
/// line (so every band is the same width) and a muted green/red body; context
/// lines take the muted tool color and non-numbered lines the system color,
/// matching the TUI. Lines never wrap — each stays one row so no phantom rows
/// appear — and the horizontal scroll area reveals long lines.
pub fn render_diff_preview(ui: &mut Ui, content: &str, colors: &ThemeColors) {
    let content = content.strip_prefix('\n').unwrap_or(content);
    let font_id = TextStyle::Monospace.resolve(ui.style().as_ref());
    let lines: Vec<&str> = content.lines().collect();
    // Pad `+`/`-` rows to the widest line so their background reads as a band.
    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut job = LayoutJob::default();
    for (index, raw) in lines.iter().enumerate() {
        let (fg, bg) = match numbered_diff_marker(raw) {
            Some('-') => (diff_line_fg(colors.diff_removed), Some(colors.diff_removed)),
            Some('+') => (diff_line_fg(colors.diff_added), Some(colors.diff_added)),
            Some(_) => (colors.tool_call, None),
            None => (colors.system_msg, None),
        };
        let mut text = (*raw).to_string();
        if bg.is_some() {
            let width = text.chars().count();
            if width < max_chars {
                text.push_str(&" ".repeat(max_chars - width));
            }
        }
        job.append(
            &text,
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: fg,
                background: bg.unwrap_or(egui::Color32::TRANSPARENT),
                ..Default::default()
            },
        );
        if index + 1 < lines.len() {
            job.append(
                "\n",
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    ..Default::default()
                },
            );
        }
    }
    ui.add_space(4.0);
    egui::Frame::default()
        .inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 4,
            bottom: 4,
        })
        .corner_radius(4.0)
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add(
                    egui::Label::new(job)
                        .selectable(true)
                        // Extend keeps every logical line on a single row, so the
                        // band stays uniform and no padded spaces wrap into a
                        // phantom row; the scroll area reveals overflow instead.
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
            });
        });
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_code_block_body_arrives_as_text() {
        let source = "```rust\nlet x = 1;\nprintln!(\"{x}\");\n```";
        let blocks = parse_markdown(source);
        assert_eq!(
            blocks.len(),
            1,
            "expected a single code block, got {blocks:?}"
        );
        match &blocks[0] {
            Block::Code { language, text } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert!(text.contains("let x = 1;"));
                assert!(text.contains("println!(\"{x}\");"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn heading_and_inline_styling() {
        let blocks = parse_markdown("# Title\n\nSome **bold** and `code`.");
        assert!(matches!(
            &blocks[0],
            Block::Heading { level, runs } if *level == 1 && runs.len() == 1
        ));
        match &blocks[1] {
            Block::Paragraph {
                runs,
                marker,
                depth,
            } => {
                assert!(marker.is_none());
                assert_eq!(*depth, 0);
                let bolds: Vec<_> = runs.iter().filter(|r| r.bold).collect();
                assert_eq!(bolds.len(), 1);
                assert_eq!(bolds[0].text, "bold");
                let codes: Vec<_> = runs.iter().filter(|r| r.code).collect();
                assert_eq!(codes.len(), 1);
                assert_eq!(codes[0].text, "code");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn ordered_and_unordered_lists_flatten() {
        let blocks = parse_markdown("- one\n- two\n\n1. first\n2. second");
        let paras: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { marker, .. } => Some(marker.clone().unwrap()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paras,
            vec![
                "•".to_string(),
                "•".to_string(),
                "1.".to_string(),
                "2.".to_string()
            ]
        );
    }

    #[test]
    fn safe_and_unsafe_links() {
        let blocks =
            parse_markdown("[ok](https://x.com) [bad](javascript:alert(1)) [mail](mailto:a@b.c)");
        let para = match &blocks[0] {
            Block::Paragraph { runs, .. } => runs,
            other => panic!("expected paragraph, got {other:?}"),
        };
        let with_url: Vec<_> = para.iter().filter_map(|r| r.url.clone()).collect();
        assert_eq!(
            with_url,
            vec!["https://x.com".to_string(), "mailto:a@b.c".to_string()]
        );
    }

    #[test]
    fn blockquote_increases_depth() {
        let blocks = parse_markdown("> quoted");
        match &blocks[0] {
            Block::Paragraph { depth, .. } => assert_eq!(*depth, 1),
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn preformatted_prefix_preserves_newlines_and_bounds() {
        // Under the bound: the whole text survives byte-for-byte.
        let text = "line one\n  line two\nline\tthree";
        let (shown, remaining) = preformatted_prefix(text, 10);
        assert_eq!(shown, text);
        assert_eq!(remaining, 0);

        // At the bound: exactly `max_lines` whole lines, newlines intact,
        // and the remainder counted in full lines (no partial line shown).
        let text = (0..50)
            .map(|i| format!("log {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (shown, remaining) = preformatted_prefix(&text, 7);
        assert_eq!(
            shown,
            (0..7)
                .map(|i| format!("log {i:02}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(remaining, 43);
        assert_eq!(shown.lines().count(), 7);

        // A trailing line without a final newline still counts as one line.
        let (shown, remaining) = preformatted_prefix("a\nb", 2);
        assert_eq!(shown, "a\nb");
        assert_eq!(remaining, 0);
        let (shown, remaining) = preformatted_prefix("a\nb", 1);
        // The truncated last line keeps no trailing newline (no phantom row).
        assert_eq!(shown, "a");
        assert_eq!(remaining, 1);

        // Zero bound and empty input stay well defined.
        assert_eq!(preformatted_prefix("a\nb", 0), (String::new(), 2));
        assert_eq!(preformatted_prefix("", 3), (String::new(), 0));
    }

    #[test]
    fn wide_tables_and_code_do_not_expand_the_transcript() {
        let long = "unbroken_content_".repeat(80);
        for text in [
            format!("```\n{long}\n```"),
            format!("| A | B |\n| --- | --- |\n| {long} | {long} |"),
        ] {
            let ctx = egui::Context::default();
            let blocks = parse_markdown(&text);
            for _ in 0..3 {
                let mut width = 0.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(320.0, 400.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        render_blocks(ui, "overflow", &blocks, &ThemeColors::default());
                        width = ui.min_rect().width();
                    },
                );
                output.textures_delta.clear();
                assert!(
                    width <= 320.0,
                    "wide content expanded its container to {width}"
                );
            }
        }
    }

    #[test]
    fn table_parses_headers_rows_and_alignment() {
        let blocks = parse_markdown("| Name | Qty |\n| :--- | ---: |\n| a | 1 |\n| b | 2 |");
        assert_eq!(blocks.len(), 1, "expected one table, got {blocks:?}");
        match &blocks[0] {
            Block::Table {
                headers,
                alignments,
                rows,
            } => {
                assert_eq!(headers.len(), 2);
                assert_eq!(headers[0][0].text, "Name");
                assert_eq!(headers[1][0].text, "Qty");
                assert_eq!(*alignments, vec![Align::Min, Align::Max]);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0][0][0].text, "a");
                assert_eq!(rows[0][1][0].text, "1");
                assert_eq!(rows[1][0][0].text, "b");
                assert_eq!(rows[1][1][0].text, "2");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn task_list_markers_render_checkboxes() {
        let blocks = parse_markdown("- [x] done\n- [ ] todo");
        let markers: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { marker, .. } => marker.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["• [x] ".to_string(), "• [ ] ".to_string()]);
    }

    #[test]
    fn footnote_reference_and_definition() {
        let blocks = parse_markdown("[^note]: The footnote body.\n\nText[^note] here.");
        let para_text = |runs: &[Run]| runs.iter().map(|r| r.text.as_str()).collect::<String>();
        // The reference is emitted as `[^note]` inline text and merged with its
        // neighbors by `push_run`, so assert on the paragraph's combined text.
        let ref_text = blocks.iter().find_map(|b| match b {
            Block::Paragraph { runs, marker, .. } if marker.is_none() => Some(para_text(runs)),
            _ => None,
        });
        assert_eq!(ref_text.as_deref(), Some("Text[^note] here."));
        // The definition becomes a paragraph prefixed with `[^note]: `.
        let def = blocks.iter().find_map(|b| match b {
            Block::Paragraph { marker, runs, .. } if marker.as_deref() == Some("[^note]: ") => {
                Some(para_text(runs))
            }
            _ => None,
        });
        assert_eq!(def.as_deref(), Some("The footnote body."));
    }

    #[test]
    fn numbered_diff_marker_matches_tui_gutters() {
        assert_eq!(numbered_diff_marker("    1 +added"), Some('+'));
        assert_eq!(numbered_diff_marker("   12 -removed"), Some('-'));
        assert_eq!(numbered_diff_marker("  123  context"), Some(' '));
        // Non-numbered and short lines are not diff rows.
        assert_eq!(numbered_diff_marker("@@ -1,3 +1,4 @@"), None);
        assert_eq!(numbered_diff_marker("short"), None);
        assert_eq!(numbered_diff_marker(""), None);
        // Column 6 must be a diff marker, not arbitrary text.
        assert_eq!(numbered_diff_marker("    1 xtext"), None);
    }

    #[test]
    fn code_layout_job_highlights_distinct_token_colors() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                let job = code_layout_job(
                    ui,
                    Some("rust"),
                    "fn main() {\n    let x: u32 = 42; // comment\n}\n",
                    &ThemeColors::default(),
                );
                let distinct: std::collections::HashSet<_> =
                    job.sections.iter().map(|s| s.format.color).collect();
                assert!(
                    distinct.len() >= 2,
                    "expected multiple token colors, got {distinct:?}"
                );
            },
        );
        out.textures_delta.clear();
    }

    #[test]
    fn inline_code_background_and_prose_line_height_per_section() {
        let style = egui::Style::default();
        let colors = ThemeColors::default();
        let code_bg = style.visuals.faint_bg_color;
        assert_ne!(
            code_bg,
            egui::Color32::TRANSPARENT,
            "the subtle inline-code background should not be clear"
        );

        let runs = vec![
            Run {
                text: "hello ".into(),
                bold: false,
                italic: false,
                strike: false,
                code: false,
                url: None,
            },
            Run {
                text: "x = 1".into(),
                bold: false,
                italic: false,
                strike: false,
                code: true,
                url: None,
            },
            Run {
                text: " world".into(),
                bold: false,
                italic: false,
                strike: false,
                code: false,
                url: None,
            },
        ];

        // Requesting a line height gives every section prose breathing room, and
        // the code run picks up the subtle background while plain runs stay clear.
        let job = build_job(
            &runs,
            &style,
            None,
            None,
            false,
            Some(PROSE_LINE_HEIGHT),
            &colors,
        );
        assert_eq!(job.sections.len(), 3);
        for (i, section) in job.sections.iter().enumerate() {
            assert_eq!(
                section.format.line_height,
                Some(PROSE_LINE_HEIGHT),
                "section {i} should inherit the prose line height"
            );
            let expected = if i == 1 {
                code_bg
            } else {
                egui::Color32::TRANSPARENT
            };
            assert_eq!(
                section.format.background, expected,
                "section {i} background"
            );
        }

        // Without a requested line height, sections keep the font's default.
        let job = build_job(&runs, &style, None, None, false, None, &colors);
        for section in &job.sections {
            assert_eq!(section.format.line_height, None);
        }
        assert_eq!(job.sections[1].format.background, code_bg);
    }

    #[test]
    fn diff_preview_bands_are_uniform_and_never_wrap() {
        let colors = ThemeColors::default();
        let content = "\n    edit_file x (-1 | +1)\n    1 - short\n    2 + a much longer added line that surely exceeds a narrow available width okay\n    3  context";
        let logical_lines = content.strip_prefix('\n').unwrap().lines().count();
        // A narrow viewport must not add phantom rows or split a band in two.
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| render_diff_preview(ui, content, &colors),
        );
        fn galleys(shape: &egui::epaint::Shape, out: &mut Vec<std::sync::Arc<egui::Galley>>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.galley.clone()),
                egui::epaint::Shape::Vec(shapes) => {
                    for s in shapes {
                        galleys(s, out);
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &out.shapes {
            galleys(&clipped.shape, &mut found);
        }
        out.textures_delta.clear();
        let galley = found
            .iter()
            .find(|g| g.job.text.contains("edit_file x"))
            .expect("diff preview galley");
        assert_eq!(
            galley.rows.len(),
            logical_lines,
            "each diff line must stay on exactly one row"
        );
        // Both banded rows share the widest width: the band is uniform.
        let band_width = galley.rows[1].rect().width();
        assert_eq!(
            galley.rows[2].rect().width(),
            band_width,
            "added and removed bands must be the same width"
        );
        // Muted green/red bodies over the theme fills; context takes the tool color.
        let bg_of = |target: egui::Color32| {
            galley
                .job
                .sections
                .iter()
                .any(|s| s.format.background == target)
        };
        assert!(bg_of(colors.diff_added) && bg_of(colors.diff_removed));
        let fg_of =
            |target: egui::Color32| galley.job.sections.iter().any(|s| s.format.color == target);
        assert!(fg_of(diff_line_fg(colors.diff_added)));
        assert!(fg_of(diff_line_fg(colors.diff_removed)));
        assert!(fg_of(colors.tool_call), "context uses the muted tool color");
    }

    #[test]
    fn heading_sizes_are_restrained() {
        assert_eq!(heading_pixel_size(1), 24.0);
        assert_eq!(heading_pixel_size(2), 21.0);
        assert_eq!(heading_pixel_size(3), 18.0);
        // Nothing outruns the level-1 size, and deeper levels step down.
        assert!(heading_pixel_size(4) < heading_pixel_size(3));
        assert!(heading_pixel_size(6) <= heading_pixel_size(4));
    }
}
