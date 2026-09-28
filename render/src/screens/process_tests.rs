use super::*;

#[test]
fn process_output_keeps_stdout_and_stderr_styles() {
    let process = bone_protocol::ProcessSnapshot {
        id: "process-1".into(),
        command: "build".into(),
        owner: "conversation:1".into(),
        running: true,
        state: bone_protocol::ProcessState::Running,
        started_at: 0,
        finished_at: None,
        stdout: "out".into(),
        stderr: "err".into(),
        exit_code: None,
        signal: None,
        error: None,
    };
    let mut theme = crate::theme::Theme::default();
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
        state: bone_protocol::ProcessState::Exited,
        started_at: 0,
        finished_at: None,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: Some(143),
        signal: Some(15),
        error: None,
    };
    let theme = crate::theme::Theme::default();
    let lines = process_lines(&process, 80, &theme);

    assert_eq!(lines[1].to_string(), "exit code: 143");
    assert_eq!(lines[2].to_string(), "signal: 15");
    assert_eq!(lines[1].spans[0].style.fg, Some(theme.palette.muted));
}

#[test]
fn touch_on_the_contextual_cancel_hint_returns_ctrl_c() {
    let process = bone_protocol::ProcessSnapshot {
        id: "process-1".into(),
        command: "build".into(),
        owner: "conversation:1".into(),
        running: true,
        state: bone_protocol::ProcessState::Running,
        started_at: 0,
        finished_at: None,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        signal: None,
        error: None,
    };
    let screen = ProcessScreen::new(process);
    let start = "running · 0s · following".chars().count() as u16;
    let key = screen
        .touch_key(4, start + 3, 80, 5)
        .expect("cancel hint is tappable");
    assert_eq!(key.code, KeyCode::Char('c'));
    assert!(key.ctrl);
    assert!(screen.touch_key(4, 0, 80, 5).is_none());
}

#[test]
fn touch_on_a_completed_process_close_hint_returns_escape() {
    let process = bone_protocol::ProcessSnapshot {
        id: "process-1".into(),
        command: "build".into(),
        owner: "conversation:1".into(),
        running: false,
        state: bone_protocol::ProcessState::Exited,
        started_at: 0,
        finished_at: None,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: Some(0),
        signal: None,
        error: None,
    };
    let screen = ProcessScreen::new(process);
    let start = "exited · 0s · following · ↑/↓ PgUp/PgDn Home/End scroll"
        .chars()
        .count() as u16;
    let key = screen
        .touch_key(4, start + 3, 80, 5)
        .expect("completed footer close hint is tappable");
    assert_eq!(key, Key::plain(KeyCode::Esc));
    assert!(screen.touch_key(3, start + 3, 80, 5).is_none());
}
