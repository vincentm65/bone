use super::*;

#[derive(Debug)]
struct TestDrop;
impl egui::DroppedFile for TestDrop {
    fn path(&self) -> &std::path::Path {
        std::path::Path::new("sample.png")
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(b"fixture".to_vec())
    }
}

#[test]
fn shortcut_close_targets_the_focused_visible_pane() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    app.selected = 0;
    app.split = true;
    app.split_tab = 1;
    app.focused_pane = Some(app.tabs[1].id);
    app.tabs[1].composer = "right draft".into();
    app.apply_shortcut(egui::Key::W, &ctx);
    assert_eq!(app.close_target, Some(app.tabs[1].id));
    assert!(!app.tabs[0].closing);
}

#[test]
fn drops_attach_only_inside_the_destination_pane() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    app.last_pointer = Some(egui::pos2(650.0, 300.0));
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen),
            dropped_files: vec![std::sync::Arc::new(TestDrop)],
            ..Default::default()
        },
        |ui| {
            egui::Panel::right("test-right")
                .default_size(380.0)
                .show(ui, |ui| {
                    app.conversation_pane(1, ui);
                });
            app.conversation_pane(0, ui);
        },
    )
    .textures_delta
    .clear();
    assert!(app.tabs[0].attachments.is_empty());
    assert_eq!(app.tabs[1].attachments.len(), 1);
}

#[test]
fn remote_connections_require_a_loopback_tunnel_endpoint() {
    for address in [
        "example.com:7878",
        "192.168.1.10:7878",
        "0.0.0.0:7878",
        "[::]:7878",
        "localhost.evil:7878",
    ] {
        assert!(daemon::local_endpoint(address).is_err(), "{address}");
    }
    for address in ["127.0.0.1:7878", "localhost:17878", "[::1]:7878"] {
        assert!(daemon::local_endpoint(address).unwrap().ip().is_loopback());
    }
}

#[test]
fn closing_busy_or_draft_tabs_requires_confirmation() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = tx;
    app.tabs[0].connected = true;
    app.tabs[0].state.busy = true;
    app.close_tab(0);
    assert_eq!(app.close_target, Some(app.tabs[0].id));
    assert!(!app.tabs[0].closing);
    assert!(rx.try_recv().is_err());
    app.close_target = None;
    app.tabs[0].state.busy = false;
    app.tabs[0].composer = "keep my draft".into();
    app.close_tab(0);
    assert!(app.close_target.is_some());
    assert_eq!(app.tabs[0].composer, "keep my draft");
    app.tabs[0].state.busy = true;
    app.finish_close_tab(0);
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::Cancel))
    ));
    assert!(matches!(rx.try_recv(), Ok(Command::Disconnect)));
}
