use super::*;

#[test]
fn process_output_keeps_stdout_and_stderr_styles() {
    let process = bone_protocol::ProcessSnapshot {
        id: "process-1".into(),
        command: "build".into(),
        owner: "conversation:1".into(),
        running: true,
        stdout: "out".into(),
        stderr: "err".into(),
        exit_code: None,
        signal: None,
        error: None,
    };
    let mut theme = crate::ui::theme::Theme::default();
    theme.palette.fg = ratatui::style::Color::Rgb(1, 2, 3);
    theme.tool_error = ratatui::style::Color::Rgb(4, 5, 6);
    let lines = process_lines(&process, 80, &theme);

    assert_eq!(lines[1].style.fg, None);
    assert_eq!(lines[1].spans[0].style.fg, Some(theme.palette.fg));
    assert_eq!(lines[2].spans[0].style.fg, Some(theme.tool_error));
}

#[test]
fn completed_process_renders_exit_and_signal_metadata() {
    let process = bone_protocol::ProcessSnapshot {
        id: "process-1".into(),
        command: "build".into(),
        owner: "conversation:1".into(),
        running: false,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: Some(143),
        signal: Some(15),
        error: None,
    };
    let theme = crate::ui::theme::Theme::default();
    let lines = process_lines(&process, 80, &theme);

    assert_eq!(lines[1].to_string(), "exit code: 143");
    assert_eq!(lines[2].to_string(), "signal: 15");
    assert_eq!(lines[1].spans[0].style.fg, Some(theme.palette.muted));
}
