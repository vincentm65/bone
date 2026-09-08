//! Minimal Markdown parsing and egui rendering for transcript rows.
//!
//! Parsing is kept pure ([`parse_markdown`]) and separated from rendering
//! ([`render_blocks`]) so the parse result can be cached in the UI layer while
//! the renderer stays a thin egui projection. Only a deliberate subset of
//! CommonMark is rendered: headings, paragraphs, inline emphasis/strike/code,
//! fenced/indented code blocks, ordered & unordered lists, block quotes and
//! horizontal rules. Links are rendered and opened only when their URL is
//! considered safe (see [`is_safe_url`]).

use eframe::egui::{
    self, Align, AsIdSalt, Color32, FontSelection, RichText, Stroke, Style, Ui, text::LayoutJob,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One contiguous run of text sharing the same inline styling.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// Only set for links whose URL passed [`is_safe_url`].
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
    Rule,
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
    if let Some(last) = runs.last_mut() {
        if last.bold == bold
            && last.italic == italic
            && last.strike == strike
            && last.code == code
            && last.url == url
        {
            last.text.push_str(text);
            return;
        }
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

    let list_depth = |list_stack: &[(bool, u64)]| {
        if list_stack.is_empty() {
            0
        } else {
            list_stack.len()
        }
    };

    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH) {
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
                    url = Some(dest_url.to_string()).filter(|u| is_safe_url(u));
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
                }
                TagEnd::CodeBlock => {
                    if let Some((language, body)) = code.take() {
                        blocks.push(Block::Code {
                            language,
                            text: body,
                        });
                    }
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
            Event::Code(c) => {
                if code.is_none() {
                    push_run(&mut runs, &c, false, false, false, true, None);
                }
            }
            Event::SoftBreak => {
                if code.is_none() {
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
            }
            Event::HardBreak => {
                if code.is_none() {
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
            }
            Event::Rule => blocks.push(Block::Rule),
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

/// Build an egui [`LayoutJob`] from a run of styled text.
fn build_job(runs: &[Run], style: &Style, size: Option<f32>) -> LayoutJob {
    let mut job = LayoutJob::default();
    for run in runs {
        let mut rich = RichText::new(run.text.clone());
        if let Some(size) = size {
            rich = rich.size(size);
        }
        if run.bold {
            rich = rich.strong();
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
        rich.append_to(&mut job, style, FontSelection::Default, Align::TOP);
    }
    job
}

/// Render a list of parsed blocks into the transcript column.
pub fn render_blocks(ui: &mut Ui, salt: impl AsIdSalt + Copy, blocks: &[Block]) {
    for (index, block) in blocks.iter().enumerate() {
        match block {
            Block::Rule => {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
            Block::Code { language, text } => render_code_block(ui, language.as_deref(), text),
            Block::Heading { level, runs } => {
                ui.add_space(6.0);
                let size = heading_pixel_size(*level);
                let job = build_job(runs, &ui.style(), Some(size));
                ui.add(egui::Label::new(job));
                ui.add_space(2.0);
            }
            Block::Paragraph {
                runs,
                marker,
                depth,
            } => {
                if *depth > 0 {
                    ui.scope(|ui| {
                        ui.indent((salt, index), |ui| {
                            render_inline(ui, runs, marker.as_deref())
                        });
                    });
                } else {
                    render_inline(ui, runs, marker.as_deref());
                }
            }
        }
    }
}

fn heading_pixel_size(level: u8) -> f32 {
    match level {
        1 => 30.0,
        2 => 24.0,
        3 => 19.0,
        4 => 17.0,
        _ => 15.0,
    }
}

/// Render the inline content of a paragraph. When no run carries a link the
/// whole paragraph is one selectable, wrapping label (the fast path). Only
/// when a link is present do we fall back to a wrapping row of segments so each
/// safe link becomes an independent, clickable widget.
fn render_inline(ui: &mut Ui, runs: &[Run], marker: Option<&str>) {
    if !runs.iter().any(|run| run.url.is_some()) {
        let style = ui.style();
        let mut job = LayoutJob::default();
        if let Some(marker) = marker {
            let marker = RichText::new(marker.to_string());
            marker.append_to(&mut job, &style, FontSelection::Default, Align::TOP);
            let gap = RichText::new("  ");
            gap.append_to(&mut job, &style, FontSelection::Default, Align::TOP);
        }
        for run in runs {
            let mut rich = RichText::new(run.text.clone());
            if run.bold {
                rich = rich.strong();
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
            rich.append_to(&mut job, &style, FontSelection::Default, Align::TOP);
        }
        ui.add(egui::Label::new(job).selectable(true));
        return;
    }

    ui.horizontal_wrapped(|ui| {
        if let Some(marker) = marker {
            ui.label(marker);
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
            let job = build_job(&segment, &ui.style(), None);
            match url {
                None => {
                    ui.add(egui::Label::new(job));
                }
                Some(url) => {
                    if ui.hyperlink_to(job, url.clone()).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::same_tab(url));
                    }
                }
            }
        }
    });
}

fn render_code_block(ui: &mut Ui, language: Option<&str>, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if let Some(language) = language {
            ui.label(RichText::new(language.to_string()).small().weak());
        }
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if ui.button(RichText::new("Copy").small()).clicked() {
                ui.ctx().copy_text(text.to_string());
            }
        });
    });
    let frame = egui::Frame::default()
        .fill(Color32::from_gray(24))
        .stroke(Stroke::new(1.0, Color32::from_gray(60)))
        .inner_margin(8.0)
        .corner_radius(4.0);
    frame.show(ui, |ui| {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(text.to_string()).monospace())
                    .selectable(true)
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
        let with_url: Vec<_> = para
            .iter()
            .filter(|r| r.url.is_some())
            .map(|r| r.url.clone().unwrap())
            .collect();
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
}
