//! Native live pane for host-managed background processes.
use super::pane_page::PanePage;
use bone_protocol::ProcessSnapshot;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const PANE_SOURCE: &str = "processes";

pub fn render(processes: &[ProcessSnapshot], selected_id: Option<&str>) -> Option<PanePage> {
    let active: Vec<_> = processes.iter().filter(|process| process.running).collect();
    if active.is_empty() {
        return None;
    }

    let rows = active
        .iter()
        .map(|process| {
            let selected = Some(process.id.as_str()) == selected_id;
            let command = process.command.replace(['\n', '\r'], " ");
            let label: String = command.chars().take(48).collect();
            let tail = process
                .stdout
                .lines()
                .last()
                .or_else(|| process.stderr.lines().last())
                .unwrap_or("starting")
                .replace(['\n', '\r'], " ");
            let tail: String = tail.chars().take(40).collect();
            let line = Line::from(vec![
                Span::styled(
                    "◑ ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::default().fg(Color::White)),
                Span::styled(format!(" — {tail}"), Style::default().fg(Color::Gray)),
            ]);
            (selected, line)
        })
        .collect();

    Some(super::selectable_pane::render(
        PANE_SOURCE,
        format!("Processes ({})", active.len()),
        rows,
    ))
}

#[cfg(test)]
mod tests {
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
        let page = render(&[process("process-1")], Some("process-1"))
            .expect("running process should render");

        assert!(page.content[0].to_string().contains("latest"));
        assert!(page.content[0].to_string().contains('›'));
        assert_eq!(page.content[0].style.bg, Some(Color::Rgb(0x3A, 0x3F, 0x4B)));
    }

    #[test]
    fn selected_process_scrolls_into_view() {
        let processes: Vec<_> = (0..10)
            .map(|index| process(&format!("process-{index}")))
            .collect();
        let page = render(&processes, Some("process-9")).unwrap();

        assert_eq!(page.scroll, 2);
        assert!(page.scroll <= page.max_scroll());
    }
}
