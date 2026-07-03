use bone::ui::render::messages::msg_to_lines;
use bone::ui::theme::Theme;
use bone::ui::tool_display::shell_row;
use ratatui::style::Color;
use ratatui::text::Line;

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn span_color(line: &Line<'static>, text: &str) -> Option<Color> {
    line.spans
        .iter()
        .find(|span| span.content.as_ref() == text)
        .and_then(|span| span.style.fg)
}

fn shell_lines(output: &str, width: u16, expanded: bool) -> Vec<Line<'static>> {
    msg_to_lines(
        &[shell_row("echo hi", output.to_string(), false)],
        &Theme::default(),
        None,
        width,
        expanded,
    )
}

#[test]
fn short_shell_output_renders_full_gutter_block() {
    let rendered = shell_lines("one\ntwo\nthree\nfour\nfive", 80, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "    shell echo hi",
            "      │ one",
            "      │ two",
            "      │ three",
            "      │ four",
            "      ╰ five",
            ""
        ]
    );
}

#[test]
fn long_shell_output_renders_head_marker_and_tail() {
    let rendered = shell_lines("1\n2\n3\n4\n5\n6", 80, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "    shell echo hi",
            "      │ 1",
            "      │ 2",
            "      │ ⋮ +2 terminal lines (ctrl+o)",
            "      │ 5",
            "      ╰ 6",
            ""
        ]
    );
}

#[test]
fn expanded_shell_output_has_no_marker() {
    let rendered = shell_lines("1\n2\n3\n4\n5\n6", 80, true)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(rendered.iter().all(|line| !line.contains("ctrl+o")));
    assert!(rendered.contains(&"      │ 4".to_string()));
}

#[test]
fn shell_tool_boilerplate_is_hidden() {
    let rendered = shell_lines(
        "exit code: 0\nstdout:\nmatch one\nmatch two\nstderr:\nwarning",
        80,
        false,
    )
    .iter()
    .map(line_text)
    .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "    shell echo hi",
            "      │ match one",
            "      │ match two",
            "      ╰ warning",
            "",
        ]
    );
}

#[test]
fn empty_stdout_does_not_render_blank_line_before_stderr() {
    let rendered = shell_lines(
        "exit code: 101\nstdout:\n\nstderr:\nerror: package not found",
        80,
        false,
    )
    .iter()
    .map(line_text)
    .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec!["    shell echo hi", "      ╰ error: package not found", "",]
    );
}

#[test]
fn narrow_shell_output_wrap_keeps_gutter() {
    let rendered = shell_lines("alpha beta gamma", 14, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "    shell".to_string(),
            "     echo hi".to_string(),
            "      │ alpha".to_string(),
            "      │ beta".to_string(),
            "      ╰ gamma".to_string(),
            "".to_string(),
        ]
    );
}

#[test]
fn wrapped_output_truncates_to_five_terminal_lines() {
    let rendered = shell_lines("alpha beta gamma delta epsilon zeta eta theta", 14, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "    shell".to_string(),
            "     echo hi".to_string(),
            "      │ alpha".to_string(),
            "      │ beta".to_string(),
            "      │ ⋮ +5 terminal lines (ctrl+o)".to_string(),
            "      │ eta".to_string(),
            "      ╰ theta".to_string(),
            "".to_string(),
        ]
    );
}

#[test]
fn shell_label_accents_program_and_separators() {
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[shell_row("cargo test && echo done", String::new(), false)],
        &theme,
        None,
        80,
        false,
    );
    let first = &lines[0];

    assert_eq!(first.spans[4].content.as_ref(), "cargo");
    assert_eq!(first.spans[4].style.fg, Some(theme.shell_program));
    assert_eq!(first.spans[8].content.as_ref(), "&&");
    assert_eq!(first.spans[8].style.fg, Some(theme.shell_separator));
    assert_eq!(first.spans[10].content.as_ref(), "echo");
    assert_eq!(first.spans[10].style.fg, Some(theme.shell_program));
}

#[test]
fn shell_label_highlights_flags_redirects_variables_paths_and_comments() {
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[shell_row(
            "FOO=bar cargo test --all >out.txt # done",
            String::new(),
            false,
        )],
        &theme,
        None,
        80,
        false,
    );
    let line = &lines[0];

    assert_eq!(span_color(line, "FOO=bar"), Some(theme.shell_variable));
    assert_eq!(span_color(line, "cargo"), Some(theme.shell_program));
    assert_eq!(span_color(line, "--all"), Some(theme.shell_flag));
    assert_eq!(span_color(line, ">"), Some(theme.shell_redirect));
    assert_eq!(span_color(line, "out.txt"), Some(theme.shell_path));
    assert_eq!(span_color(line, "# done"), Some(theme.shell_comment));
}

#[test]
fn shell_label_highlights_strings_and_variables() {
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[shell_row(
            "echo \"$HOME\" '$USER' $SHELL ${TERM}",
            String::new(),
            false,
        )],
        &theme,
        None,
        80,
        false,
    );
    let line = &lines[0];

    assert_eq!(span_color(line, "echo"), Some(theme.shell_program));
    assert_eq!(span_color(line, "\"$HOME\""), Some(theme.shell_string));
    assert_eq!(span_color(line, "'$USER'"), Some(theme.shell_string));
    assert_eq!(span_color(line, "$SHELL"), Some(theme.shell_variable));
    assert_eq!(span_color(line, "${TERM}"), Some(theme.shell_variable));
}

#[test]
fn heredoc_body_is_not_accented_as_program() {
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[shell_row("cat << EOFhelloEOF", String::new(), false)],
        &theme,
        None,
        80,
        false,
    );
    let body = lines
        .iter()
        .find(|line| line_text(line).contains("hello"))
        .expect("heredoc body line");

    assert!(
        body.spans
            .iter()
            .all(|span| span.style.fg != Some(theme.shell_program))
    );
}

#[test]
fn error_shell_output_uses_error_gutter() {
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[shell_row("false", "nope".to_string(), true)],
        &theme,
        None,
        80,
        false,
    );

    let output = lines
        .iter()
        .find(|line| line_text(line).starts_with("      ╰"))
        .unwrap();
    assert_eq!(output.spans[0].style.fg, Some(Color::Rgb(224, 80, 80)));
}

#[test]
fn non_ascii_shell_commands_do_not_panic_the_lexer() {
    for cmd in [
        "echo héllo",
        "grep -n '│' file.rs",
        "echo ⋮ && ls",
        "echo $foé",
        "rüst --flag ./päth/file.rs",
    ] {
        let row = shell_row(cmd, String::new(), false);
        msg_to_lines(&[row], &Theme::default(), None, 80, false);
    }
}
