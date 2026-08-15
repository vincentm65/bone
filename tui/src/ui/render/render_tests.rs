use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

fn desired_height(
    renderer: &Renderer,
    input: &crate::ui::input::InputState,
    pages: &[crate::ui::pane_page::PanePage],
    autocomplete: Option<&crate::ui::autocomplete::AutocompleteState>,
    running: usize,
) -> u16 {
    renderer.desired_height(
        &PaneSizing {
            input,
            prompt: None,
            pages,
            active_page: 0,
            autocomplete,
            running,
        },
        40,
    )
}

#[test]
fn terminal_background_writes_exact_osc_sequences() {
    let mut output = Vec::new();
    assert!(write_terminal_background(&mut output, Some(Color::Rgb(1, 2, 3))).unwrap());
    assert_eq!(output, b"\x1b]11;rgb:01/02/03\x1b\\");

    output.clear();
    assert!(write_terminal_background(&mut output, Some(Color::Blue)).unwrap());
    assert_eq!(output, b"\x1b]11;rgb:24/72/c8\x1b\\");

    output.clear();
    assert!(!write_terminal_background(&mut output, None).unwrap());
    assert!(!write_terminal_background(&mut output, Some(Color::Indexed(1))).unwrap());
    assert!(output.is_empty());

    write_terminal_background_reset(&mut output).unwrap();
    assert_eq!(output, b"\x1b]111\x1b\\");
}

#[test]
fn max_viewport_height_reserves_a_row_when_possible() {
    assert_eq!(max_viewport_height(0), 1);
    assert_eq!(max_viewport_height(1), 1);
    assert_eq!(max_viewport_height(2), 1);
    assert_eq!(max_viewport_height(3), 2);
    assert_eq!(max_viewport_height(24), 23);
    assert_eq!(max_viewport_height(u16::MAX), u16::MAX - 1);
}

#[test]
fn initial_viewport_height_clamps_minimum_rows_to_terminal() {
    assert_eq!(initial_viewport_height(0), 1);
    assert_eq!(initial_viewport_height(1), 1);
    assert_eq!(initial_viewport_height(2), 1);
    assert_eq!(initial_viewport_height(3), 2);
    assert_eq!(initial_viewport_height(4), MIN_ROWS);
    assert_eq!(initial_viewport_height(24), MIN_ROWS);
}

#[test]
fn desired_viewport_height_tracks_input_panes_completion_and_running_rows() {
    let renderer = Renderer::new();
    let mut input = crate::ui::input::InputState::default();
    let empty = desired_height(&renderer, &input, &[], None, 0);

    input.buffer = "first\nsecond\nthird".into();
    input.cursor_pos = input.buffer.chars().count();
    let multiline = desired_height(&renderer, &input, &[], None, 0);
    assert!(multiline > empty);

    input.reset();
    assert_eq!(desired_height(&renderer, &input, &[], None, 0), empty);

    let page = crate::ui::pane_page::PanePage {
        source: "test".into(),
        title: "test".into(),
        content: vec![Line::raw("one"), Line::raw("two")],
        visible_rows: 2,
        scroll: 0,
    };
    let pane_open = desired_height(&renderer, &input, &[page], None, 0);
    assert!(pane_open > empty);

    let completion = crate::ui::autocomplete::AutocompleteState::new(vec![(
        "command".into(),
        "description".into(),
    )]);
    let completion_open = desired_height(&renderer, &input, &[], Some(&completion), 0);
    assert_eq!(completion_open, empty + completion.visible_rows());

    let running = desired_height(&renderer, &input, &[], None, 2);
    assert_eq!(running, empty + 2);
}

#[test]
fn consecutive_scrollback_separators_are_deduplicated() {
    let mut renderer = Renderer::new();
    let blank = [Line::raw("")];

    assert_eq!(renderer.dedup_scrollback_blanks(&blank).len(), 1);
    assert!(renderer.dedup_scrollback_blanks(&blank).is_empty());

    let content = [Line::raw("next")];
    assert_eq!(renderer.dedup_scrollback_blanks(&content).len(), 1);
    assert_eq!(renderer.dedup_scrollback_blanks(&blank).len(), 1);
}

/// Reproduces the panic shape from counting wrap height at a wider width than
/// the `insert_before` temp buffer (viewport) actually has.
#[test]
fn render_scrollback_lines_survives_underallocated_height() {
    // Content that wraps to more rows at width 99 than at width 100.
    let lines = vec![
        Line::from("x".repeat(100)),
        Line::from("hello"),
        Line::from("z".repeat(150)),
    ];
    // Pre-fix row count at the *wrong* (wider) width — this is what the
    // old `term.size().width` path did when the viewport lagged a Resize.
    let under_height = logical_lines_row_count(&lines, 100);
    let correct_height = logical_lines_row_count(&lines, 99);
    assert!(
        correct_height > under_height,
        "fixture must need more rows at the narrower width ({correct_height} > {under_height})"
    );

    let area = Rect {
        x: 0,
        y: 0,
        width: 99,
        height: under_height,
    };
    let mut buf = Buffer::empty(area);
    // Must not panic even though the allocated height is too short for the
    // true wrap at buf width — the renderer clamps to remaining rows.
    render_scrollback_lines(&lines, &mut buf);
}

#[test]
fn logical_lines_row_count_uses_wrap_width() {
    let line = Line::from("x".repeat(100));
    assert_eq!(logical_lines_row_count(std::slice::from_ref(&line), 100), 1);
    assert_eq!(logical_lines_row_count(&[line], 99), 2);
}

#[test]
fn assistant_markdown_preserves_soft_line_breaks_for_terminal_output() {
    let content = "exit code: 0\nstdout:\n\nrunning 2 tests\ntest first ... ok\ntest second ... ok";
    let rendered = markdown::render_markdown(content, 80, &Theme::default())
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "exit code: 0",
            "stdout:",
            "",
            "running 2 tests",
            "test first ... ok",
            "test second ... ok",
        ]
    );
}

#[test]
fn muted_markdown_colors_plain_text_and_preserves_emphasis() {
    let theme = Theme::default();
    let rendered = markdown::render_markdown_muted("plain line\n*Recap: done* line", 80, &theme);

    let text: Vec<String> = rendered
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref().to_string())
                .collect()
        })
        .collect();
    let first = text
        .iter()
        .find(|line| !line.trim().is_empty())
        .expect("a non-empty rendered line");
    assert_eq!(first, "plain line");

    // Every span in a muted render carries an explicit foreground.
    for line in &rendered {
        for span in &line.spans {
            if !span.content.as_ref().is_empty() {
                assert_eq!(
                    span.style.fg,
                    Some(theme.palette.muted),
                    "span {:?} should carry the muted base color",
                    span.content.as_ref()
                );
            }
        }
    }

    // The emphasis span is muted AND italic.
    let italic_span = rendered
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref().contains("Recap: done"))
        .expect("emphasis span rendered");
    assert_eq!(italic_span.style.fg, Some(theme.palette.muted));
    assert!(
        italic_span.style.add_modifier.contains(ratatui::style::Modifier::ITALIC),
        "emphasis span should be italic"
    );
}

#[test]
fn muted_markdown_keeps_explicit_span_colors() {
    let theme = Theme::default();
    // Inline code carries its own fg and must not be repainted muted.
    let rendered = markdown::render_markdown_muted("text with `code` here", 80, &theme);
    let code_span = rendered
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref() == "code")
        .expect("inline code span rendered");
    assert_eq!(code_span.style.fg, Some(theme.markdown_inline_code));
}

#[test]
fn system_message_renders_as_muted_markdown() {
    let theme = Theme::default();
    let msg = crate::chat::Message::system("*Recap: fixed the auth bug*");
    let lines = super::messages::msg_to_lines(&[msg], &theme, None, 80, true);
    assert!(!lines.is_empty());
    let recap_span = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref().contains("Recap: fixed the auth bug"))
        .expect("recap span rendered");
    assert_eq!(recap_span.style.fg, Some(theme.palette.muted));
    assert!(
        recap_span.style.add_modifier.contains(ratatui::style::Modifier::ITALIC),
        "system message emphasis should render italic"
    );
}
