use super::*;

fn process(id: &str) -> ProcessSnapshot {
    ProcessSnapshot {
        id: id.into(),
        command: "long build".into(),
        owner: "conversation:1".into(),
        running: true,
        stdout: "first\nlatest".into(),
        stderr: String::new(),
        exit_code: None,
        signal: None,
        error: None,
    }
}

#[test]
fn running_process_renders_selected_row_with_latest_output() {
    let mut theme = Theme::default();
    theme.palette.selection = ratatui::style::Color::Blue;
    let page = render(&theme, &[process("process-1")], Some("process-1"))
        .expect("running process should render");

    assert!(page.content[0].to_string().contains("latest"));
    assert!(page.content[0].to_string().contains('›'));
    assert_eq!(page.content[0].style.bg, Some(theme.palette.selection));
}

#[test]
fn selected_process_scrolls_into_view() {
    let processes: Vec<_> = (0..10)
        .map(|index| process(&format!("process-{index}")))
        .collect();
    let page = render(&Theme::default(), &processes, Some("process-9")).unwrap();

    assert_eq!(page.scroll, 2);
    assert!(page.scroll <= page.max_scroll());
}
