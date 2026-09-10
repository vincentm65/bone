use super::*;

fn dialog_text(app: &mut DesktopApp, ctx: &egui::Context, screen: egui::Rect) -> String {
    let mut text = String::new();
    // Areas need an initial sizing pass before their centered position settles.
    for _ in 0..3 {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| app.key_capture_dialog(ui.ctx()),
        );
        text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(t)
                    if shape
                        .clip_rect
                        .intersect(screen)
                        .contains_rect(egui::Rect::from_min_size(t.pos, t.galley.size())) =>
                {
                    Some(t.galley.text())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        output.drop_without_applying_deltas();
    }
    text
}

#[test]
fn key_input_modal_shows_owning_tasks_options_even_when_panes_are_hidden() {
    use bone_protocol::{Component, PaneContent, PaneLineSpec, PaneSpanSpec, ViewDiff};
    for width in [900.0, 360.0] {
        let ctx = egui::Context::default();
        let mut app = DesktopApp::open(ctx.clone(), false, None);
        app.add_tab(Intent::New, &ctx);
        app.selected = 0; // The pending question belongs to a background task.
        app.panes_visible = false;
        app.tabs[1].state.ready = true;
        let pane = |title: &str| {
            Component::float_from_pane_content(&PaneContent {
                source: "custom-menu-source".into(),
                title: title.into(),
                lines: vec![
                    PaneLineSpec::Plain("Which checks should run?".into()),
                    PaneLineSpec::Spans {
                        spans: vec![PaneSpanSpec {
                            text: "> [x] Unit tests".into(),
                            fg: Some("accent".into()),
                            modifiers: vec!["bold".into()],
                        }],
                        bg: Some("#303040".into()),
                    },
                    PaneLineSpec::Plain("  [ ] Integration tests".into()),
                    PaneLineSpec::Plain("Custom: type an answer".into()),
                    PaneLineSpec::Plain("Preview: Fast local checks".into()),
                ],
                visible_rows: 8,
                scroll: 0,
            })
        };
        app.tabs[0].state.view.components.push(pane("Wrong task"));
        app.tabs[1].state.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Upsert {
                component: pane("Choose checks"),
            },
        });
        app.tabs[1]
            .state
            .reduce(RuntimeEvent::KeyRequest { id: 77 });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0));
        let text = dialog_text(&mut app, &ctx, screen);
        for expected in [
            "Choose checks",
            "Which checks should run?",
            "> [x] Unit tests",
            "  [ ] Integration tests",
            "Custom: type an answer",
            "Preview: Fast local checks",
        ] {
            assert!(
                text.contains(expected),
                "missing {expected:?} at width {width}: {text}"
            );
        }
        assert!(!text.contains("Wrong task"), "{text}");
        assert!(!text.contains("Press any key to send it."), "{text}");

        // Selection/custom input changes arrive as view updates, not native
        // interpretations of option text or the tool arguments.
        let mut updated = pane("Choose checks");
        if let Component::Float { lines, .. } = &mut updated {
            lines[1] = PaneLineSpec::Plain("  [x] Unit tests".into());
            lines[2] = PaneLineSpec::Plain("> [x] Integration tests".into());
            lines[3] = PaneLineSpec::Plain("Custom: smoke tests".into());
        }
        app.tabs[1].state.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Upsert { component: updated },
        });
        let text = dialog_text(&mut app, &ctx, screen);
        assert!(text.contains("> [x] Integration tests"), "{text}");
        assert!(text.contains("Custom: smoke tests"), "{text}");
        assert!(!text.contains("> [x] Unit tests"), "{text}");

        // Removing the authoritative pane must not leave stale options behind.
        app.tabs[1].state.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Remove {
                id: "custom-menu-source".into(),
            },
        });
        let text = dialog_text(&mut app, &ctx, screen);
        assert!(!text.contains("Unit tests"), "{text}");
        assert!(text.contains("Press any key to send it."), "{text}");
    }
}

#[test]
fn key_input_modal_owns_navigation_before_app_shortcuts() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.tabs[0].connected = true;
    app.tabs[0].state.ready = true;
    app.tabs[0].composer = "keep this draft".into();
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = tx;
    app.keymap = keymap::Keymap::default();
    app.keymap.bindings.push(keymap::Binding {
        key: "<C-k>".into(),
        action: "test-action".into(),
    });
    for (id, key, modifiers) in [
        (
            1,
            egui::Key::K,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        ),
        (2, egui::Key::ArrowDown, egui::Modifiers::NONE),
        (3, egui::Key::Space, egui::Modifiers::NONE),
        (4, egui::Key::Enter, egui::Modifiers::NONE),
        (5, egui::Key::Escape, egui::Modifiers::NONE),
        (
            6,
            egui::Key::P,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        ),
        (7, egui::Key::Tab, egui::Modifiers::SHIFT),
        (8, egui::Key::ArrowDown, egui::Modifiers::ALT),
        (
            9,
            egui::Key::C,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        ),
        (10, egui::Key::A, egui::Modifiers::NONE),
    ] {
        app.tabs[0].state.reduce(RuntimeEvent::KeyRequest { id });
        assert!(app.modal_open());
        let mut events = vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
        ];
        if key == egui::Key::A {
            events.push(egui::Event::Text("a".into()));
        }
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            assert_eq!(ui.input(|i| i.modifiers), modifiers);
            app.handle_keymap(ui);
            app.handle_shortcuts(ui);
            app.handle_pane_keys(ui);
            app.key_capture_dialog(ui.ctx());
            assert!(
                !ui.input(|i| i.key_pressed(key)),
                "captured key leaked to other controls"
            );
            assert!(!ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))));
            app.conversation_pane(0, ui);
        })
        .drop_without_applying_deltas();
        let expected = keys::key_event(key, modifiers);
        assert!(
            matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::KeyReply { id: reply_id, key: reply }))
            if reply_id == id && reply.code == expected.code && reply.char == expected.char
                && reply.ctrl == expected.ctrl && reply.alt == expected.alt && reply.shift == expected.shift)
        );
        assert!(!app.modal_open());
        assert!(rx.try_recv().is_err());
        assert!(!app.palette.open);
        assert_eq!(app.tabs[0].composer, "keep this draft");
    }
}
