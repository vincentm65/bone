use super::*;

fn painted_text(out: &mut egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn collect(shape: &egui::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(t) => found.push((
                t.galley.job.text.clone(),
                t.galley.rect.translate(t.pos.to_vec2()),
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, found);
                }
            }
            _ => {}
        }
    }
    out.textures_delta.clear();
    let mut texts = vec![];
    for shape in &out.shapes {
        collect(&shape.shape, &mut texts);
    }
    texts
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        )),
        events,
        ..Default::default()
    }
}

fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn agent_viewer_reads_and_cancels_only_the_origin_after_switching_tabs() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (origin_tx, mut origin_rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = first_tx;
    app.tabs[1].commands = origin_tx;
    for (i, tab) in app.tabs.iter_mut().enumerate() {
        tab.connected = true;
        tab.state.ready = true;
        let mut job = live_pane::tests::job();
        job.events = vec![bone_protocol::JobEventSnapshot::TextDelta {
            text: format!("Transcript from tab {i}"),
        }];
        tab.state.jobs = vec![job];
    }
    let origin = app.tabs[1].id;
    app.apply_ui_request(origin, UiRequest::OpenJob("job-1".into()));
    app.selected = 0;
    let mut texts = vec![];
    for _ in 0..4 {
        let mut out = ctx.run_ui(input(vec![]), |ui| app.job_view_dialog(ui.ctx()));
        texts = painted_text(&mut out);
    }
    assert!(
        texts
            .iter()
            .any(|(text, _)| text.contains("Transcript from tab 1"))
    );
    assert!(
        !texts
            .iter()
            .any(|(text, _)| text.contains("Transcript from tab 0"))
    );
    let cancel = texts
        .iter()
        .find(|(text, _)| text == "Cancel job")
        .unwrap()
        .1
        .center();
    for pressed in [true, false] {
        ctx.run_ui(input(pointer(cancel, pressed)), |ui| {
            app.job_view_dialog(ui.ctx())
        })
        .textures_delta
        .clear();
    }
    assert!(first_rx.try_recv().is_err());
    assert!(
        matches!(origin_rx.try_recv(), Ok(Command::Send(RuntimeCommand::CancelJob { id })) if id == "job-1")
    );
    app.tabs[1].state.jobs.clear();
    ctx.run_ui(input(vec![]), |ui| app.job_view_dialog(ui.ctx()))
        .textures_delta
        .clear();
    assert!(
        app.job_view.is_none(),
        "must not use the identically named job in the selected tab"
    );
}

#[test]
fn live_pages_and_agent_clicks_are_independent_between_conversations() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true, None);
    app.add_demo_tab(&ctx);
    app.tabs[0].live_pane.select("task_list".into());
    let origin = app.tabs[1].id;
    let mut texts = vec![];
    let palette = theme::Palette::default();
    let mut render = |events| {
        let mut out = ctx.run_ui(input(events), |ui| {
            ui.columns(2, |columns| {
                for (tab, column) in app.tabs.iter_mut().zip(columns) {
                    column.push_id(tab.id, |ui| {
                        if let Some(id) = tab.live_pane.render(
                            ui,
                            &tab.state.view,
                            &tab.state.jobs,
                            &palette,
                            200.0,
                        ) {
                            tab.pending_ui.push(UiRequest::OpenJob(id));
                        }
                    });
                }
            });
        });
        painted_text(&mut out)
    };
    for _ in 0..3 {
        texts = render(vec![]);
    }
    assert!(
        texts
            .iter()
            .any(|(text, _)| text.contains("Build reusable live panes"))
    );
    let tasks_tab = texts
        .iter()
        .rfind(|(text, _)| text == "Tasks (1/3)")
        .unwrap()
        .1
        .center();
    render(pointer(tasks_tab, true));
    let texts = render(pointer(tasks_tab, false));
    assert_eq!(
        texts
            .iter()
            .filter(|(text, _)| text.contains("Build reusable live panes"))
            .count(),
        2
    );
    let agents_tab = texts
        .iter()
        .rfind(|(text, _)| text == "Agents (2)")
        .unwrap()
        .1
        .center();
    render(pointer(agents_tab, true));
    let texts = render(pointer(agents_tab, false));
    let agent = texts
        .iter()
        .find(|(text, _)| text.contains("Researcher · Review tool presentation"))
        .unwrap()
        .1
        .center();
    render(pointer(agent, true));
    render(pointer(agent, false));
    let _ = render;
    app.drain_local();
    assert_eq!(app.job_view.as_ref().unwrap().tab_id, origin);
    assert_eq!(app.tabs[0].live_pane.selected, Some("task_list".into()));
    assert_eq!(
        app.tabs[1].live_pane.selected,
        Some(live_pane::PageId::Agents)
    );
}

#[test]
fn live_pane_above_composer_does_not_clip_short_agent_lists() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true, None);
    let colors = theme::ThemeColors::default();
    let palette = theme::Palette::default();
    for height in [360.0, 700.0] {
        for frame in 0..4 {
            let mut raw = input(vec![]);
            raw.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(500.0, height),
            ));
            let mut out = ctx.run_ui(raw, |ui| {
                app.tabs[0].body(ui, &colors, Some(&palette), None, false, true);
            });
            out.textures_delta.clear();
            if frame < 3 {
                continue;
            }
            if let Some(shape) = out.shapes.iter().find(|shape| match &shape.shape {
                egui::Shape::Text(t) => t.galley.job.text.contains("Reviewer ·"),
                _ => false,
            }) && let egui::Shape::Text(t) = &shape.shape
            {
                let text_rect = t.galley.rect.translate(t.pos.to_vec2());
                assert!(
                    shape.clip_rect.contains_rect(text_rect),
                    "agent row clipped: {text_rect:?} clip={:?}",
                    shape.clip_rect
                );
            }
        }
    }
}
