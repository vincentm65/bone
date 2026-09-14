//! Layout and interaction checks, with optional software-rendered PNG previews.
use super::*;
use bone_protocol::{CatalogItem, ConfigPage, SettingDefinition, UsageBucket};
use std::collections::HashMap;

fn fixture(ctx: &egui::Context) -> DesktopApp {
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.tabs[0].connected = true;
    app.config = Some(tests::sample_config(1, "example", &[("example", "model")]));
    app.config_schema = Some(ConfigSchema {
        pages: [
            (
                "general",
                "General",
                vec![
                    (
                        "show_reasoning",
                        "Show reasoning",
                        "bool",
                        serde_json::json!(true),
                    ),
                    (
                        "max_iterations",
                        "Maximum iterations",
                        "number",
                        serde_json::json!(100),
                    ),
                    (
                        "approval",
                        "Approval mode",
                        "enum",
                        serde_json::json!("safe"),
                    ),
                ],
            ),
            (
                "appearance",
                "Appearance",
                vec![("theme", "Color theme", "enum", serde_json::json!("dark"))],
            ),
            (
                "tools",
                "Tools",
                vec![(
                    "shell",
                    "Run shell commands",
                    "bool",
                    serde_json::json!(true),
                )],
            ),
        ]
        .into_iter()
        .map(|(namespace, title, fields)| ConfigPage {
            namespace: namespace.into(),
            title: title.into(),
            pages: vec![],
            fields: fields
                .into_iter()
                .map(|(key, label, kind, value)| SettingDefinition {
                    path: format!("{namespace}.{key}"),
                    key: key.into(),
                    label: label.into(),
                    value_type: kind.into(),
                    options: if key == "theme" {
                        vec!["dark".into(), "light".into()]
                    } else {
                        vec!["safe".into(), "danger".into()]
                    },
                    default: value,
                    value: None,
                    integer: Some(true),
                    min: if kind == "number" { Some(1.0) } else { None },
                    max: if kind == "number" { Some(1000.0) } else { None },
                    kind: None,
                    reload_behavior: "next_turn".into(),
                })
                .collect(),
        })
        .collect(),
    });
    app.catalog = Some(CatalogSnapshot { revision: "preview".into(), items: [
        ("Workspace memory", "Remember project conventions and useful context between tasks.", true, true),
        ("Browser tools", "Inspect pages and interact with websites from your tasks.", false, false),
        ("Git helpers", "Review repository history and compare changes.", true, false),
        ("Task templates", "Start recurring work with reusable instructions.", false, false),
        ("Documentation search", "Search your project's documentation from a conversation.", false, false),
    ].into_iter().map(|(name, description, installed, update_available)| CatalogItem {
        name: name.into(), description: description.into(), installed, update_available,
        kind: "tool".into(), version: Some("1.2.0".into()), author: Some("Bone community".into()),
        long_description: Some("Keep the context that matters close at hand. Configure this extension after installation in Settings.".into()),
        permissions: vec!["Read workspace files".into()], ..Default::default()
    }).collect() });
    let buckets: Vec<_> = (1..=7)
        .map(|day| UsageBucket {
            label: format!("2026-09-{day:02}"),
            prompt_tokens: [12000, 34000, 28000, 51000, 16000, 43000, 26000][day - 1],
            completion_tokens: 3500,
            cached_tokens: 8000,
            cost: 0.0,
            request_count: 12,
        })
        .collect();
    app.stats = Some(UsageStatsSnapshot {
        started_at: None,
        ended_at: None,
        total: Default::default(),
        by_model_today: vec![],
        by_model_7d: vec![bone_protocol::ProviderUsage {
            provider: "Example provider".into(),
            model: "Coding model".into(),
            prompt_tokens: 210000,
            completion_tokens: 24500,
            cached_tokens: 56000,
            cost: 0.0,
            request_count: 84,
        }],
        by_model_4w: vec![],
        by_model_all: vec![],
        daily: buckets.clone(),
        weekly: buckets.clone(),
        monthly: buckets.clone(),
        all_time: buckets.clone(),
        yearly: buckets.clone(),
        hourly_today: vec![],
        hourly_7d: vec![],
        hourly_4w: vec![],
        hourly_all: vec![],
        daily_activity: buckets,
    });
    app.stats_mode = 1;
    app.stats_refreshed = Some(Instant::now());
    app
}

fn show(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    name: &str,
    width: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    show_at(app, ctx, name, width, 800.0, events)
}

fn show_at(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    name: &str,
    width: f32,
    height: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            ui.label("Bone workspace");
            match name {
                "Settings" => app.config_dialog(ui.ctx()),
                "Plugins" => app.plugins_dialog(ui.ctx()),
                "Usage" => app.stats_dialog(ui.ctx()),
                _ => unreachable!(),
            }
        },
    );
    ctx.data_mut(|data| {
        let textures = data.get_temp_mut_or_default::<HashMap<egui::TextureId, egui::ColorImage>>(
            egui::Id::new("preview-textures"),
        );
        apply_textures(textures, &output);
    });
    output.textures_delta.clear();
    output
}

fn text_rect(output: &egui::FullOutput, label: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(t) if t.galley.text() == label => {
            Some(t.galley.rect.translate(t.pos.to_vec2()))
        }
        _ => None,
    })
}

#[test]
fn utility_screens_fit_and_keep_navigation_visible() {
    for (width, height) in [
        (1200.0, 800.0),
        (720.0, 600.0),
        (390.0, 800.0),
        (520.0, 400.0),
    ] {
        for name in ["Settings", "Plugins", "Usage"] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            app.open_utility(name);
            let mut last = None;
            for _ in 0..4 {
                let output = show_at(&mut app, &ctx, name, width, height, vec![]);
                last = Some(output);
            }
            let output = last.unwrap();
            let screen = ctx.content_rect();
            let surface = ctx
                .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("last-surface-rect")))
                .unwrap();
            assert!(
                screen.contains_rect(surface),
                "{name} bounds at {width}: {surface:?}"
            );
            for label in ["Settings", "Plugins", "Usage"] {
                let rect = text_rect(&output, label)
                    .unwrap_or_else(|| panic!("missing {label} in {name}"));
                assert!(
                    screen.contains_rect(rect),
                    "{name} navigation clipped at {width}: {rect:?}"
                );
            }
            for shape in &output.shapes {
                if let egui::Shape::Rect(rect) = &shape.shape
                    && rect.fill == ctx.style_of(ctx.theme()).visuals.panel_fill
                    && rect.rect.width() > 200.0
                {
                    assert!(
                        screen.expand(1.0).contains_rect(rect.rect),
                        "{name} surface escapes {width}: {:?}",
                        rect.rect
                    );
                }
            }
            if let Ok(directory) = std::env::var("BONE_UI_PREVIEW_DIR") {
                std::fs::create_dir_all(&directory).unwrap();
                let textures = ctx
                    .data(|data| {
                        data.get_temp::<HashMap<egui::TextureId, egui::ColorImage>>(egui::Id::new(
                            "preview-textures",
                        ))
                    })
                    .unwrap();
                write_preview(
                    &ctx,
                    output,
                    &textures,
                    width as u32,
                    height as u32,
                    &PathBuf::from(directory).join(format!("{}-{width}.png", name.to_lowercase())),
                );
            }
        }
    }
}

#[test]
fn narrow_navigation_keeps_body_below_the_nav_row() {
    // Regression: in a narrow Ui the trailing slot must occupy a single
    // control-height row. A bare `with_layout(right_to_left + Center)` seeds
    // the child `min_rect` at the vertical centre of the remaining panel, so
    // the body of the Plugins/Usage side panels was dropped to the middle.
    for width in [300.0_f32, 390.0] {
        let ctx = egui::Context::default();
        let mut body_y = None;
        let mut nav_y = None;
        for _ in 0..2 {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1000.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    assert!(ui.available_width() < 420.0);
                    crate::surface::navigation(ui, "Usage", |_| {});
                    nav_y = Some(ui.cursor().min.y);
                    let response = ui.label("Body marker");
                    body_y = Some(response.rect.min.y);
                },
            )
            .textures_delta
            .clear();
        }
        let nav_y = nav_y.unwrap();
        let body_y = body_y.unwrap();
        assert!(
            body_y >= nav_y - 1.0 && body_y < 120.0,
            "narrow body at y={body_y} (nav bottom {nav_y}, width {width}); \
             expected it just below the nav row, not at the vertical middle"
        );
    }
}

#[test]
fn settings_layout_stays_still_with_overflowing_content() {
    for (width, height) in [
        (1200.0, 800.0),
        (1365.0, 767.0),
        (1001.0, 701.0),
        (843.0, 701.0),
        (900.0, 800.0),
        (720.0, 600.0),
        (390.0, 800.0),
        (520.0, 400.0),
    ] {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            let page = &mut app.config_schema.as_mut().unwrap().pages[0];
            for index in 0..30 {
                page.fields.push(SettingDefinition {
                    path: format!("general.extra_{index}"),
                    key: format!("extra_{index}"),
                    label: format!("Additional setting {index}"),
                    value_type: "string".into(),
                    options: Vec::new(),
                    default: serde_json::json!("value"),
                    value: None,
                    integer: None,
                    min: None,
                    max: None,
                    kind: None,
                    reload_behavior: String::new(),
                });
            }
            ctx.set_pixels_per_point(scale);
            app.show_config = true;
            let mut previous: Option<egui::Rect> = None;
            for frame in 0..40 {
                let _output = show_at(&mut app, &ctx, "Settings", width, height, vec![]);
                let surface = ctx
                    .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("last-surface-rect")))
                    .unwrap();
                if frame >= 20
                    && let Some(previous_surface) = previous
                {
                    let drift = (surface.min.x - previous_surface.min.x)
                        .abs()
                        .max((surface.max.x - previous_surface.max.x).abs())
                        .max((surface.min.y - previous_surface.min.y).abs())
                        .max((surface.max.y - previous_surface.max.y).abs());
                    assert!(
                        drift < 1.0,
                        "settings jump at {width}×{height}, scale {scale}, frame {frame}: {drift}; {previous_surface:?} -> {surface:?}"
                    );
                }
                previous = Some(surface);
            }
        }
    }
}

#[test]
fn plugins_layout_stays_still_with_overflowing_content() {
    for (width, height) in [
        (1200.0, 800.0),
        (1365.0, 767.0),
        (1001.0, 701.0),
        (843.0, 701.0),
        (900.0, 800.0),
        (720.0, 600.0),
        (390.0, 800.0),
        (520.0, 400.0),
    ] {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            let catalog = app.catalog.as_mut().unwrap();
            catalog.items[0].long_description =
                Some("A long description that should wrap within the details column. ".repeat(60));
            for index in 0..30 {
                let mut item = catalog.items[1].clone();
                item.name = format!("Extension {index} with a longer name");
                catalog.items.push(item);
            }
            ctx.set_pixels_per_point(scale);
            app.show_plugins = true;
            let mut previous: Option<egui::Rect> = None;
            for frame in 0..40 {
                let _output = show_at(&mut app, &ctx, "Plugins", width, height, vec![]);
                let bounds = ctx
                    .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("last-surface-rect")))
                    .unwrap();
                let positions = bounds;
                if frame >= 20
                    && let Some(previous) = previous
                {
                    let drift = (positions.min.x - previous.min.x)
                        .abs()
                        .max((positions.max.x - previous.max.x).abs());
                    assert!(
                        drift < 1.0,
                        "plugins jump at {width}×{height}, scale {scale}, frame {frame}: {drift}"
                    );
                }
                previous = Some(positions);
            }
        }
    }
}

#[test]
fn plugins_install_button_sends_only_the_selected_item() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = tx;
    app.show_plugins = true;
    let mut output = show(&mut app, &ctx, "Plugins", 1200.0, vec![]);
    for _ in 0..3 {
        output = show(&mut app, &ctx, "Plugins", 1200.0, vec![]);
    }
    let pos = text_rect(&output, "Install").unwrap().center();
    for pressed in [true, false] {
        show(
            &mut app,
            &ctx,
            "Plugins",
            1200.0,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let command = rx.try_recv().expect("install should send a request");
    match command {
        Command::Send(RuntimeCommand::HostRequest {
            request: HostRequest::CatalogApply { actions, .. },
            ..
        }) => {
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].name, "Browser tools");
            assert_eq!(actions[0].action, CatalogActionKind::Install);
        }
        _ => panic!("expected a catalog apply request"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn utility_escape_closes_without_discarding_the_conversation_draft() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    app.tabs[0].composer = "keep my draft".into();
    app.show_config = true;
    for _ in 0..3 {
        show(&mut app, &ctx, "Settings", 1200.0, vec![]);
    }
    show(
        &mut app,
        &ctx,
        "Settings",
        1200.0,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(!app.show_config);
    assert_eq!(app.tabs[0].composer, "keep my draft");
}

#[test]
fn settings_search_crosses_categories_and_validation_persists() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    app.show_config = true;
    app.config_ui.query = "theme".into();
    let mut output = show(&mut app, &ctx, "Settings", 1200.0, vec![]);
    for _ in 0..3 {
        output = show(&mut app, &ctx, "Settings", 1200.0, vec![]);
    }
    assert!(text_rect(&output, "Color theme").is_some());
    assert!(text_rect(&output, "Maximum iterations").is_none());
    app.config_ui.query.clear();
    app.config_edits
        .insert("general.max_iterations".into(), "0".into());
    for _ in 0..3 {
        output = show(&mut app, &ctx, "Settings", 1200.0, vec![]);
        assert!(text_rect(&output, "must be at least 1").is_some());
    }
}

#[test]
fn usage_details_fill_the_width_and_have_readable_activity_labels() {
    let models: Vec<_> = [
        ("Small local model", "Local", 18450, 2600, 47),
        ("Large coding model", "Provider A", 12345678, 8500000, 1234),
        (
            "a-long-model-name-that-must-not-squeeze-the-numeric-columns",
            "Provider B",
            754321,
            400000,
            281,
        ),
    ]
    .into_iter()
    .map(
        |(model, provider, tokens, cached, requests)| bone_protocol::ProviderUsage {
            model: model.into(),
            provider: provider.into(),
            prompt_tokens: tokens,
            completion_tokens: 0,
            cached_tokens: cached,
            request_count: requests,
            cost: 0.0,
        },
    )
    .collect();
    let hourly: Vec<_> = (0..24)
        .map(|hour| bone_protocol::HourUsage {
            hour,
            prompt_tokens: if (8..=18).contains(&hour) {
                18000 + (hour * 7919) % 75000
            } else {
                0
            },
            completion_tokens: 0,
            cached_tokens: 0,
            request_count: if (8..=18).contains(&hour) { 19 } else { 0 },
        })
        .collect();
    let mut activity = Vec::new();
    for (month, days) in [(6, 30), (7, 31), (8, 31), (9, 10)] {
        for day in 1..=days {
            activity.push(UsageBucket {
                label: format!("2026-{month:02}-{day:02}"),
                prompt_tokens: if day % 6 == 0 {
                    0
                } else {
                    (day * 17479 + month * 9337) % 100000
                },
                completion_tokens: 0,
                cached_tokens: 0,
                cost: 0.0,
                request_count: 12,
            });
        }
    }
    for width in [1040.0, 420.0] {
        let ctx = egui::Context::default();
        let app = fixture(&ctx);
        let heat = app.stats_heat_scale();
        let mut textures = HashMap::new();
        let mut last = None;
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::Frame::new().inner_margin(24).show(ui, |ui| {
                        ui.set_width(width - 48.0);
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        ui.heading("Models");
                        stats::render_models(ui, &models);
                        ui.add_space(24.0);
                        ui.heading("Activity patterns");
                        stats::render_activity(
                            ui,
                            &hourly,
                            &activity,
                            &heat,
                            ui.visuals().hyperlink_color,
                        );
                    });
                },
            );
            apply_textures(&mut textures, &output);
            output.textures_delta.clear();
            last = Some(output);
        }
        let output = last.unwrap();
        if let Ok(directory) = std::env::var("BONE_UI_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            write_preview(
                &ctx,
                output.clone(),
                &textures,
                width as u32,
                1400,
                &PathBuf::from(directory).join(format!("usage-details-{width}.png")),
            );
        }
        for label in [
            "12,345,678",
            "1,234",
            "Usage by hour",
            "Daily activity",
            "Mon",
            "Tue",
            "Wed",
            "Thu",
            "Fri",
            "Sat",
            "Sun",
            "Jul",
            "Tokens per day",
        ] {
            let rect =
                text_rect(&output, label).unwrap_or_else(|| panic!("missing {label:?} at {width}"));
            assert!(
                ctx.content_rect().contains_rect(rect),
                "clipped {label:?} at {width}: {rect:?}"
            );
        }
        if width > 620.0 {
            let header = text_rect(&output, "Cache hit").unwrap();
            assert!(
                header.right() > width - 50.0,
                "table shrank to its contents: {header:?}"
            );
            let value = text_rect(&output, "69%").unwrap();
            assert!(
                (value.right() - header.right()).abs() < 1.0,
                "numeric columns are not aligned: header {header:?}, value {value:?}"
            );
        }
        let large = text_rect(&output, "Large coding model").unwrap();
        let small = text_rect(&output, "Small local model").unwrap();
        assert!(
            large.top() < small.top(),
            "models should be sorted by token usage"
        );
    }
}

// Render egui's actual tessellated output without requiring a display server.
// This is opt-in, so normal test runs only check layout and interactions.
fn apply_textures(
    textures: &mut HashMap<egui::TextureId, egui::ColorImage>,
    output: &egui::FullOutput,
) {
    for (id, deltas) in &output.textures_delta.set {
        for delta in deltas {
            let egui::ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                let texture = textures.get_mut(id).unwrap();
                for row in 0..image.size[1] {
                    for col in 0..image.size[0] {
                        texture.pixels[(y + row) * texture.size[0] + x + col] =
                            image.pixels[row * image.size[0] + col];
                    }
                }
            } else {
                textures.insert(*id, image.as_ref().clone());
            }
        }
    }
}

fn write_preview(
    ctx: &egui::Context,
    output: egui::FullOutput,
    textures: &HashMap<egui::TextureId, egui::ColorImage>,
    width: u32,
    height: u32,
    path: &std::path::Path,
) {
    let mut image = image::RgbaImage::from_pixel(width, height, image::Rgba([20, 21, 25, 255]));
    let cross = |a: egui::Vec2, b: egui::Vec2| a.x * b.y - a.y * b.x;
    for clipped in ctx.tessellate(output.shapes, output.pixels_per_point) {
        let egui::epaint::Primitive::Mesh(mesh) = clipped.primitive else {
            continue;
        };
        let Some(texture) = textures.get(&mesh.texture_id) else {
            continue;
        };
        for triangle in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [
                mesh.vertices[triangle[0] as usize],
                mesh.vertices[triangle[1] as usize],
                mesh.vertices[triangle[2] as usize],
            ];
            let area = cross(b.pos - a.pos, c.pos - a.pos);
            if area.abs() < 0.0001 {
                continue;
            }
            let bounds = egui::Rect::from_points(&[a.pos, b.pos, c.pos])
                .intersect(clipped.clip_rect)
                .intersect(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width as f32, height as f32),
                ));
            for y in bounds.top().max(0.0) as u32..(bounds.bottom().ceil() as u32).min(height) {
                for x in bounds.left().max(0.0) as u32..(bounds.right().ceil() as u32).min(width) {
                    let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    let w = [
                        cross(b.pos - p, c.pos - p) / area,
                        cross(c.pos - p, a.pos - p) / area,
                        cross(a.pos - p, b.pos - p) / area,
                    ];
                    if w.iter().any(|v| *v < 0.0) {
                        continue;
                    }
                    let uv = a.uv.to_vec2() * w[0] + b.uv.to_vec2() * w[1] + c.uv.to_vec2() * w[2];
                    let tx = (uv.x * texture.size[0] as f32)
                        .clamp(0.0, (texture.size[0] - 1) as f32)
                        as usize;
                    let ty = (uv.y * texture.size[1] as f32)
                        .clamp(0.0, (texture.size[1] - 1) as f32)
                        as usize;
                    let texel = texture.pixels[ty * texture.size[0] + tx].to_array();
                    let colors = [a.color.to_array(), b.color.to_array(), c.color.to_array()];
                    let src: [f32; 4] = std::array::from_fn(|i| {
                        (colors[0][i] as f32 * w[0]
                            + colors[1][i] as f32 * w[1]
                            + colors[2][i] as f32 * w[2])
                            * texel[i] as f32
                            / 255.0
                    });
                    let dest = image.get_pixel_mut(x, y);
                    for channel in 0..3 {
                        dest[channel] = (src[channel]
                            + dest[channel] as f32 * (1.0 - src[3] / 255.0))
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
    image.save(path).unwrap();
}
