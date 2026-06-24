use bone::ui::render::markdown::render_markdown;
use bone::ui::render::safe_markdown_prefix_end;
use ratatui::style::Modifier;

// ---------------------------------------------------------------------------
// Tests for safe_markdown_prefix_end
// ---------------------------------------------------------------------------

#[test]
fn streaming_prefix_holds_paragraph_until_block_boundary() {
    let content = "Hello\n";
    assert_eq!(safe_markdown_prefix_end(content), 0);
}

#[test]
fn streaming_prefix_holds_text_without_trailing_newline() {
    assert_eq!(safe_markdown_prefix_end("Hello"), 0);
}

#[test]
fn streaming_prefix_flushes_completed_paragraph() {
    let content = "Hello\n\nWorld";
    assert_eq!(safe_markdown_prefix_end(content), "Hello\n\n".len());
}

#[test]
fn streaming_prefix_holds_fenced_code_until_closing_fence() {
    let content = "Intro\n```rust\nfn main() {}\n";
    assert_eq!(safe_markdown_prefix_end(content), 0);
}

#[test]
fn streaming_prefix_releases_fenced_code_after_closing_fence() {
    let content = "Intro\n```rust\nfn main() {}\n```\n";
    assert_eq!(safe_markdown_prefix_end(content), content.len());
}

#[test]
fn streaming_prefix_holds_trailing_pipe_table() {
    let content = "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n";
    assert_eq!(safe_markdown_prefix_end(content), "Intro\n\n".len());
}

#[test]
fn streaming_prefix_releases_table_after_blank_line_ends_it() {
    let content = "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\n\n";
    assert_eq!(safe_markdown_prefix_end(content), content.len());
}

#[test]
fn streaming_prefix_releases_table_after_non_table_line_ends_it() {
    let content = "Intro\n\n| Name | Age |\n| ---- | --- |\n| Ada | 36 |\nNext\n";
    assert_eq!(
        safe_markdown_prefix_end(content),
        content.len() - "Next\n".len()
    );
}

#[test]
fn streaming_prefix_holds_one_pipe_line_until_next_line_disambiguates() {
    let content = "Use a | b\n";
    assert_eq!(safe_markdown_prefix_end(content), 0);
}

#[test]
fn streaming_prefix_releases_pipe_looking_non_table_text() {
    let content = "Use a | b\nNext\n\n";
    assert_eq!(safe_markdown_prefix_end(content), content.len());
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

fn streamed_text(chunks: &[&str], width: usize) -> Vec<String> {
    let mut content = String::new();
    let mut inserted = Vec::new();
    let mut stable_source = 0;
    // Mirrors Renderer::flush_fragment: each flush renders only the new
    // block-complete slice and re-inserts the seam blank that render_markdown
    // trims at fragment edges.
    let mut flush = |end: usize, content: &str, inserted: &mut Vec<String>| {
        if end <= stable_source {
            return;
        }
        let mut rendered = rendered_text(&content[stable_source..end], width);
        if !rendered.is_empty() && stable_source > 0 {
            rendered.insert(0, String::new());
        }
        inserted.append(&mut rendered);
        stable_source = end;
    };
    for chunk in chunks {
        content.push_str(chunk);
        flush(safe_markdown_prefix_end(&content), &content, &mut inserted);
    }
    flush(content.len(), &content, &mut inserted);
    inserted
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
    assert_eq!(code.style.fg, Some(ratatui::style::Color::Gray));
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
