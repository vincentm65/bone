use bone::ui::render::markdown::render_markdown as render_markdown_themed;
use bone::ui::theme::Theme;

fn render_markdown(content: &str, width: u16) -> Vec<ratatui::text::Line<'static>> {
    let theme = Theme::default();
    render_markdown_themed(content, width, &theme)
}
use bone::ui::render::safe_markdown_prefix_end;
use ratatui::style::{Color, Modifier};

fn unusual_theme() -> Theme {
    let mut theme = Theme::default();
    theme.markdown_marker = Color::Red;
    theme.markdown_heading = Color::Green;
    theme.markdown_link = Color::Yellow;
    theme.markdown_inline_code = Color::Blue;
    theme.markdown_rule = Color::Magenta;
    theme.markdown_table_border = Color::Cyan;
    theme.markdown_table_header = Color::LightRed;
    theme
}

#[test]
fn streaming_prefix_boundaries() {
    let completed_paragraph = "Hello\n\n".len();
    for (name, content, expected) in [
        ("paragraph", "Hello\n", 0),
        ("unterminated text", "Hello", 0),
        ("completed paragraph", "Hello\n\nWorld", completed_paragraph),
        ("open fence", "Intro\n```rust\nfn main() {}\n", 0),
        (
            "closed fence",
            "Intro\n```rust\nfn main() {}\n```\n",
            "Intro\n```rust\nfn main() {}\n```\n".len(),
        ),
        (
            "open table",
            "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n",
            "Intro\n\n".len(),
        ),
        (
            "table ending in blank line",
            "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n\n",
            "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n\n".len(),
        ),
        (
            "table ending in text",
            "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\nNext\n",
            "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n".len(),
        ),
        ("ambiguous pipe", "Use a | b\n", 0),
        (
            "disambiguated pipe",
            "Use a | b\nNext\n\n",
            "Use a | b\nNext\n\n".len(),
        ),
    ] {
        assert_eq!(safe_markdown_prefix_end(content, 0), expected, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Tests for markdown rendering
// ---------------------------------------------------------------------------

fn rendered_text(markdown: &str, width: usize) -> Vec<String> {
    render_markdown(markdown, width as u16)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn flush_streamed_text(
    content: &str,
    width: usize,
    end: usize,
    stable_source: &mut usize,
    inserted: &mut Vec<String>,
) {
    if end <= *stable_source {
        return;
    }
    let mut rendered = rendered_text(&content[*stable_source..end], width);
    if !rendered.is_empty() && *stable_source > 0 {
        rendered.insert(0, String::new());
    }
    inserted.append(&mut rendered);
    *stable_source = end;
}

fn streamed_text(chunks: &[&str], width: usize) -> Vec<String> {
    let mut content = String::new();
    let mut inserted = Vec::new();
    let mut stable_source = 0;
    // Mirrors Renderer::flush_fragment: each flush renders only the new
    // block-complete slice and re-inserts the seam blank that render_markdown
    // trims at fragment edges.
    for chunk in chunks {
        content.push_str(chunk);
        let end = safe_markdown_prefix_end(&content, stable_source);
        flush_streamed_text(&content, width, end, &mut stable_source, &mut inserted);
    }
    flush_streamed_text(
        &content,
        width,
        content.len(),
        &mut stable_source,
        &mut inserted,
    );
    inserted
}

#[test]
fn line_comment_scope_does_not_leak_into_next_code_line() {
    // The newlines syntax set only closes line-scoped contexts (# comments)
    // on an actual \n; the renderer must highlight with terminators or the
    // comment color bleeds into every following line.
    let lines = render_markdown("```python\n# a comment\nreturn x\n```", 80);
    let comment_fg = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.contains("a comment")))
        .and_then(|l| l.spans.iter().find(|s| s.content.contains("a comment")))
        .and_then(|s| s.style.fg)
        .expect("comment span has a color");
    let return_span = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.contains("return"))
        .expect("return span exists");
    assert_ne!(
        return_span.style.fg,
        Some(comment_fg),
        "code after a comment keeps its own color"
    );
}

#[test]
fn structural_markdown_roles_use_application_theme() {
    let theme = unusual_theme();
    let markdown = "# Heading\n\n> marker\n\n[link](https://example.com) and `code`\n\n---\n\n| Head |\n|---|\n| Cell |";
    let lines = render_markdown_themed(markdown, 80, &theme);
    let spans: Vec<_> = lines.iter().flat_map(|line| line.spans.iter()).collect();

    for (content, color) in [
        ("Heading", theme.markdown_heading),
        ("> ", theme.markdown_marker),
        ("link", theme.markdown_link),
        ("code", theme.markdown_inline_code),
        ("Head", theme.markdown_table_header),
    ] {
        assert!(
            spans
                .iter()
                .any(|span| span.content == content && span.style.fg == Some(color)),
            "missing themed span {content:?} with {color:?}: {lines:?}"
        );
    }
    let rule = render_markdown_themed("---", 20, &theme);
    assert_eq!(rule[0].style.fg, Some(theme.markdown_rule));
    assert!(spans.iter().any(|span| {
        span.content.contains('│') && span.style.fg == Some(theme.markdown_table_border)
    }));
}

#[test]
fn fenced_syntax_colors_are_independent_of_markdown_roles() {
    let theme = unusual_theme();
    let lines = render_markdown_themed("```rust\nfn main() {}\n```", 80, &theme);
    let function = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("main"))
        .expect("highlighted function span");
    assert!(function.style.fg.is_some());
    assert!(
        ![
            theme.markdown_marker,
            theme.markdown_heading,
            theme.markdown_link,
            theme.markdown_inline_code,
            theme.markdown_rule,
            theme.markdown_table_border,
            theme.markdown_table_header,
        ]
        .contains(&function.style.fg.unwrap())
    );
}

#[test]
fn wrapped_prefixes_keep_marker_theme_without_character_indexing() {
    let theme = unusual_theme();
    for markdown in [
        "> alpha beta gamma delta",
        "- alpha beta gamma delta",
        "12. alpha beta gamma delta",
    ] {
        let lines = render_markdown_themed(markdown, 12, &theme);
        assert!(lines.len() > 1, "expected wrapping: {lines:?}");
        assert!(lines.iter().all(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.style.fg == Some(theme.markdown_marker))
        }));
    }
}

#[test]
fn narrow_table_preserves_inline_span_style() {
    let theme = unusual_theme();
    let lines = render_markdown_themed(
        "| Value | Other |\n|---|---|\n| `code` | text |",
        12,
        &theme,
    );
    let code = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.style.fg == Some(theme.markdown_inline_code))
        .expect("inline code style survives narrow table truncation");
    assert_eq!(code.style.fg, Some(theme.markdown_inline_code));
}

#[test]
fn heading_text_stays_on_heading_line() {
    // pulldown_cmark strips the # syntax — it emits Text("Heading"), not "# Heading".
    assert_eq!(rendered_text("# Heading", 80), vec!["Heading"]);
}

#[test]
fn top_level_headings_receive_stronger_style() {
    let h1 = render_markdown("# Heading", 80);
    let h3 = render_markdown("### Heading", 80);
    assert!(
        h1[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(
        !h3[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn web_link_text_includes_destination() {
    assert_eq!(
        rendered_text("Go to [GitHub](https://github.com).", 80),
        vec!["Go to GitHub - https://github.com."]
    );
}

#[test]
fn local_link_renders_target_instead_of_ambiguous_label() {
    assert_eq!(
        rendered_text("[render](/tmp/project/src/render.rs:20)", 80),
        vec!["/tmp/project/src/render.rs:20"]
    );
}

#[test]
fn lists_render_markers() {
    assert_eq!(
        rendered_text("- one\n- two\n\n1. first\n2. second", 80),
        vec!["- one", "- two", "", "1. first", "2. second"]
    );
}

#[test]
fn colon_paragraph_preserves_markdown_separation() {
    assert_eq!(
        rendered_text(
            "Searching:\n\n- use rg\n- read files\n\nTools:\n\n- read_file",
            80
        ),
        vec![
            "Searching:",
            "",
            "- use rg",
            "- read files",
            "",
            "Tools:",
            "",
            "- read_file"
        ]
    );
}

#[test]
fn unordered_list_wrapped_lines_keep_item_indent() {
    assert_eq!(
        rendered_text("- alpha beta gamma", 12),
        vec!["- alpha beta", "  gamma"]
    );
}

#[test]
fn ordered_list_wrapped_lines_keep_item_indent() {
    assert_eq!(
        rendered_text("1. alpha beta gamma", 12),
        vec!["1. alpha", "   beta", "   gamma"]
    );
}

#[test]
fn fenced_code_renders_content() {
    assert_eq!(
        rendered_text("```rust\nfn main() {}\n```", 80),
        vec!["  fn main() {}"]
    );
}

#[test]
fn table_renders_aligned() {
    let md = "| Name  | Age |\n|-------|-----|\n| Alice | 30  |\n| Bob   | 25  |";
    let lines = rendered_text(md, 80);
    assert!(
        lines.len() >= 5,
        "expected >= 5 lines, got {}: {lines:?}",
        lines.len()
    );
    assert!(lines[1].contains("Name"));
    assert!(lines[1].contains("Age"));
    assert!(lines[3].contains("Alice"));
    assert!(lines[4].contains("Bob"));
}

#[test]
fn table_output_fits_narrow_width() {
    let md = "| Field | Description |\n|---|---|\n| Name | a very long description value |";
    let lines = rendered_text(md, 24);
    assert!(
        lines
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24),
        "table exceeded available width: {lines:?}"
    );
}

#[test]
fn table_fallback_fits_width_smaller_than_frame_overhead() {
    let md = "| A | B | C |\n|---|---|---|\n| long | value | here |";
    let lines = rendered_text(md, 6);
    assert!(
        lines
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 6),
        "fallback table exceeded available width: {lines:?}"
    );
}

#[test]
fn table_preserves_inline_code_style() {
    let lines = render_markdown("| Value |\n|---|\n| `code` |", 80);
    let code = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == "code")
        .expect("code cell should be present");
    assert_eq!(code.style.fg, Some(Theme::default().markdown_inline_code));
}

#[test]
fn markdown_fenced_table_renders_as_table() {
    let md = "```markdown\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
    let lines = rendered_text(md, 80);
    assert!(lines.first().is_some_and(|line| line.starts_with('┌')));
    assert!(lines.iter().any(|line| line.contains("│ 1")));
}

#[test]
fn strikethrough_text_is_rendered() {
    assert_eq!(
        rendered_text("This is ~~deleted~~ text.", 80),
        vec!["This is deleted text."]
    );
    let lines = render_markdown("This is ~~deleted~~ text.", 80);
    let deleted = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == "deleted")
        .expect("deleted span should be present");
    assert!(
        deleted.style.add_modifier.contains(Modifier::CROSSED_OUT),
        "strikethrough should set CROSSED_OUT"
    );
}

#[test]
fn streaming_blocks_render_the_same_as_completed_message() {
    let chunks = [
        "Summary:\n",
        "\n- first\n",
        "- second\n\n",
        "```rust\nlet x = 1;\n",
        "```\n\n",
        "| A | B |\n|---|---|\n| 1 | 2 |\n\n",
        "Done.",
    ];
    let complete = chunks.concat();
    assert_eq!(streamed_text(&chunks, 80), rendered_text(&complete, 80));
}

#[test]
fn block_quote_has_prefix_on_each_line() {
    let md = "> first line\n> second line";
    let lines = rendered_text(md, 80);
    assert!(lines.first().is_some_and(|l| l.starts_with("> ")));
}

#[test]
fn block_quote_separated_by_blank_line() {
    // Two separate block quotes with a blank line between them.
    // The blank line is outside both block quotes - no quote marker.
    let md = "> first\n\n> second";
    let lines = rendered_text(md, 80);
    assert_eq!(lines, vec!["> first", "", "> second"]);
}

#[test]
fn block_quote_with_explicit_blank_line() {
    // Single block quote with an explicit blank line inside.
    // > first
    // >
    // > second
    let md = "> first\n>\n> second";
    let lines = rendered_text(md, 80);
    assert!(lines.len() >= 3, "expected >= 3 lines, got {lines:?}");
    assert!(lines[0].starts_with("> first"));
    assert!(lines.last().unwrap().starts_with("> second"));
}

#[test]
fn nested_block_quote_has_multiple_prefixes() {
    let md = ">> nested";
    let lines = rendered_text(md, 80);
    assert!(lines.first().is_some_and(|l| l.starts_with("> > ")));
}

#[test]
fn block_quote_no_trailing_blank_marker() {
    let md = "> first line\n> second line";
    let lines = rendered_text(md, 80);
    assert!(
        !lines.iter().any(|l| l == "> "),
        "should not have extra blockquote blank line: {lines:?}"
    );
}

#[test]
fn block_quote_wrap_uses_prefix_on_continuation() {
    let md = "> Be very concise. Prefer short, direct answers. No fluff, no filler, no unnecessary explanation.";
    let lines = rendered_text(md, 60);
    // Every wrapped line should start with the quote prefix.
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with("> "),
            "line {i}: expected quote prefix, got: |{line}|"
        );
    }
    // No trailing marker-only line.
    assert!(
        !lines.iter().any(|l| l.trim() == ">"),
        "should not have marker-only trailing line: {lines:?}"
    );
}
