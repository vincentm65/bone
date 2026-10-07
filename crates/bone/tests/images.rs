//! Full image path: composer -> RPC upload -> durable PNG -> provider HTTP.
use base64::{Engine, engine::general_purpose::STANDARD};
use bone_client::Client;
use bone_core::{Core, scripting};
use bone_proto::methods::*;
use bone_server::Server;
use bone_tui::{Headless, RunOptions};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(10);
struct Request {
    body: Value,
    reply: oneshot::Sender<String>,
}
impl Request {
    fn answer(self, text: &str) {
        self.reply
            .send(format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({ "choices": [{ "delta": { "content": text } }] })
            ))
            .unwrap();
    }
}
struct Env {
    config: tempfile::TempDir,
    work: tempfile::TempDir,
    server: Server,
    client: Arc<Client>,
    requests: mpsc::UnboundedReceiver<Request>,
    png: Vec<u8>,
}
impl Env {
    async fn new(supports_images: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0; 4096];
                    let body = loop {
                        let n = socket.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                            let len: usize = header
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .unwrap()
                                .trim()
                                .parse()
                                .unwrap();
                            if buf.len() >= i + 4 + len {
                                break serde_json::from_slice(&buf[i + 4..i + 4 + len]).unwrap();
                            }
                        }
                    };
                    let (reply, response) = oneshot::channel();
                    if tx.send(Request { body, reply }).is_err() {
                        return;
                    }
                    let Ok(body) = response.await else {
                        return;
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        let config = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(config.path().join("core.lua"), format!(
            "bone.config.providers.fake = {{ base_url = '{url}', model = 'vision-test', supports_images = {supports_images} }}\nbone.config.compact = {{ keep = 1, auto = false }}"
        )).unwrap();
        let server = Server::new(Arc::new(Core::from_loaded(
            scripting::load_with(config.path(), &|_| None).unwrap(),
        )));
        let (client, _events) = Client::new(server.connect_in_process());
        client.initialize("image-tests").await.unwrap();
        let png = bone_media::from_rgba(
            3,
            2,
            &[
                255, 0, 10, 255, 0, 255, 20, 255, 3, 4, 5, 0, 6, 7, 8, 255, 9, 10, 11, 255, 12, 13,
                14, 255,
            ],
        )
        .unwrap()
        .bytes;
        std::fs::write(work.path().join("screen capture.png"), &png).unwrap();
        Self {
            config,
            work,
            server,
            client: Arc::new(client),
            requests,
            png,
        }
    }
    async fn tui(&self) -> Headless {
        Headless::start(
            self.server.connect_in_process(),
            RunOptions {
                clipboard_executable: None,
                cwd: self.work.path().to_string_lossy().into_owned(),
                config_dir: Some(self.config.path().to_owned()),
                resume: None,
                reload_core: false,
            },
            100,
            24,
        )
        .await
        .unwrap()
    }
    async fn begin(&mut self, tui: &mut Headless) -> (String, Request) {
        tui.type_text("first message");
        tui.press("enter").unwrap();
        let request = self.request(tui).await;
        (self.id().await, request)
    }
    async fn enqueue(
        &self,
        id: &str,
        text: &str,
        images: Vec<bone_proto::types::ImageAttachment>,
    ) -> u64 {
        self.client
            .request::<QueueAdd>(QueueAddParams {
                session_id: id.into(),
                text: text.into(),
                images,
                mode: QueueMode::Next,
            })
            .await
            .unwrap()
            .id
            .unwrap()
    }
    async fn request(&mut self, tui: &mut Headless) -> Request {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            tui.settle(Duration::from_millis(10)).await;
            if let Ok(request) = self.requests.try_recv() {
                return request;
            }
            assert!(tokio::time::Instant::now() < deadline, "{}", tui.screen());
        }
    }
    async fn attach(&self, tui: &mut Headless) {
        let before = tui.images().len();
        tui.attach_file(self.work.path().join("screen capture.png"));
        self.wait_images(tui, before + 1).await;
    }
    async fn wait_images(&self, tui: &mut Headless, count: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while tui.images().len() != count {
            tui.settle(Duration::from_millis(10)).await;
            assert!(tokio::time::Instant::now() < deadline, "{}", tui.screen());
        }
        assert!(!tui.screen().contains("Lua error"), "{}", tui.screen());
    }
    async fn id(&self) -> String {
        self.client.request::<SessionList>(Empty {}).await.unwrap()[0]
            .session_id
            .clone()
    }
    fn assert_image(&self, message: &Value, expected_text: Option<&str>) {
        assert_eq!(message["role"], "user");
        let parts = message["content"].as_array().expect("multimodal content");
        let image = parts.iter().find(|p| p["type"] == "image_url").unwrap();
        let data = image["image_url"]["url"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        assert_eq!(STANDARD.decode(data).unwrap(), self.png);
        if let Some(text) = expected_text {
            assert_eq!(
                parts.iter().find(|p| p["type"] == "text").unwrap()["text"],
                text
            );
        }
    }
}

#[tokio::test]
async fn jpeg_file_is_normalized_by_core_and_reaches_provider() {
    let mut env = Env::new(true).await;
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(7, 5)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();
    env.png = bone_media::normalize(&jpeg).unwrap().bytes;
    let path = env.work.path().join("photo.jpg");
    std::fs::write(&path, jpeg).unwrap();
    let mut tui = env.tui().await;
    tui.attach_file(path);
    env.wait_images(&mut tui, 1).await;
    assert_eq!(tui.images()[0].mime_type, "image/png");
    assert_eq!((tui.images()[0].width, tui.images()[0].height), (7, 5));
    tui.press("enter").unwrap();
    let request = env.request(&mut tui).await;
    env.assert_image(
        request.body["messages"].as_array().unwrap().last().unwrap(),
        None,
    );
    request.answer("JPEG received.");
    tui.wait_for(WAIT, |s| s.contains("JPEG received."))
        .await
        .unwrap();
}

#[tokio::test]
async fn image_only_send_persists_and_survives_restart_and_fork() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    env.attach(&mut tui).await;
    let image = tui.images()[0].clone();
    assert!(tui.screen().contains("screen capture.png"));
    tui.press("enter").unwrap();
    let request = env.request(&mut tui).await;
    env.assert_image(
        request.body["messages"].as_array().unwrap().last().unwrap(),
        None,
    );
    request.answer("I see the screenshot.");
    tui.wait_for(WAIT, |s| s.contains("I see the screenshot."))
        .await
        .unwrap();
    assert!(tui.images().is_empty());
    let id = env.id().await;
    let stored = env
        .client
        .request::<SessionMessages>(SessionRef {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    let bone_proto::types::ChatMessage::User { content, images } = &stored.messages[0] else {
        panic!()
    };
    assert!(content.is_empty());
    assert_eq!(images, std::slice::from_ref(&image));
    assert!(images[0].data.is_none());
    let saved = std::fs::read_to_string(
        env.config
            .path()
            .join("sessions")
            .join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert!(
        !saved.contains(&STANDARD.encode(&env.png)),
        "pixels leaked into session JSONL"
    );
    let fork = env
        .client
        .request::<SessionFork>(SessionForkParams {
            session_id: id.clone(),
            before_turn: None,
        })
        .await
        .unwrap();
    env.client
        .request::<SessionDelete>(SessionRef { session_id: id })
        .await
        .unwrap();
    let restarted = Server::new(Arc::new(Core::from_loaded(
        scripting::load_with(env.config.path(), &|_| None).unwrap(),
    )));
    let (client, _) = Client::new(restarted.connect_in_process());
    client.initialize("restart-test").await.unwrap();
    let data = client
        .request::<AttachmentRead>(AttachmentReadParams { id: image.id })
        .await
        .unwrap();
    assert_eq!(STANDARD.decode(data.data).unwrap(), env.png);
    let messages = client
        .request::<SessionMessages>(SessionRef {
            session_id: fork.session_id,
        })
        .await
        .unwrap();
    assert_eq!(messages.messages[0], stored.messages[0]);
}

#[tokio::test]
async fn slash_attach_multiple_detach_and_failed_send_preserve_draft() {
    let mut env = Env::new(false).await;
    let mut tui = env.tui().await;
    tui.type_text("/attach screen capture.png");
    tui.press("enter").unwrap();
    env.wait_images(&mut tui, 1).await;
    tui.type_text("/attach \"screen capture.png\"");
    tui.press("enter").unwrap();
    env.wait_images(&mut tui, 2).await;
    assert_eq!(tui.images()[0].id, tui.images()[1].id);
    tui.type_text("/detach 1");
    tui.press("enter").unwrap();
    assert_eq!(tui.images().len(), 1);
    tui.type_text("explain this error");
    tui.press("enter").unwrap();
    tui.wait_for(WAIT, |s| s.contains("does not support images"))
        .await
        .unwrap();
    assert_eq!(tui.prompt_text(), "explain this error");
    assert_eq!(tui.images().len(), 1);
    assert!(env.requests.try_recv().is_err());
    tui.press("esc").unwrap();
    assert!(tui.images().is_empty());
    env.attach(&mut tui).await;
    tui.type_text("/detach all");
    tui.press("enter").unwrap();
    assert!(tui.images().is_empty());
}

#[tokio::test]
async fn queued_image_edit_reaches_next_model_call() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    tui.type_text("first");
    tui.press("enter").unwrap();
    let first = env.request(&mut tui).await;
    env.attach(&mut tui).await;
    tui.type_text("check screenshot");
    tui.press("enter").unwrap();
    tui.settle(Duration::from_millis(30)).await;
    let id = env.id().await;
    let queued = env
        .client
        .request::<SessionMessages>(SessionRef {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(queued.queue[0].images.len(), 1);
    tui.press("up").unwrap();
    assert_eq!(tui.images().len(), 1);
    tui.type_text(" changed");
    tui.press("enter").unwrap();
    tui.settle(Duration::from_millis(30)).await;
    let queued = env
        .client
        .request::<SessionMessages>(SessionRef { session_id: id })
        .await
        .unwrap();
    assert_eq!(queued.queue[0].text, "check screenshot changed");
    assert_eq!(queued.queue[0].images.len(), 1);
    first.answer("First response.");
    let second = env.request(&mut tui).await;
    env.assert_image(
        second.body["messages"].as_array().unwrap().last().unwrap(),
        Some("check screenshot changed"),
    );
    second.answer("Screenshot checked.");
    tui.wait_for(WAIT, |s| s.contains("Screenshot checked."))
        .await
        .unwrap();
}

#[tokio::test]
async fn drafts_follow_sessions_and_late_reads_do_not_attach_to_new_sessions() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    tui.type_text("first");
    tui.press("enter").unwrap();
    env.request(&mut tui).await.answer("done");
    tui.wait_for(WAIT, |s| s.contains("done")).await.unwrap();
    let original = env.id().await;
    tui.attach_file(env.work.path().join("screen capture.png"));
    tui.press("ctrl+n").unwrap();
    tui.settle(Duration::from_millis(200)).await;
    assert!(tui.images().is_empty());
    tui.open_session(original);
    env.wait_images(&mut tui, 1).await;
    tui.attach_file(env.work.path().join("screen capture.png"));
    tui.press("esc").unwrap();
    tui.settle(Duration::from_millis(200)).await;
    assert!(
        tui.images().is_empty(),
        "cleared drafts resurrected a late image"
    );
}

#[tokio::test]
async fn compaction_summarizer_receives_images_and_retains_original_transcript() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    env.attach(&mut tui).await;
    tui.type_text("look at screenshot");
    tui.press("enter").unwrap();
    env.request(&mut tui)
        .await
        .answer("A red and green screenshot.");
    tui.wait_for(WAIT, |s| s.contains("A red and green screenshot."))
        .await
        .unwrap();
    tui.type_text("next question");
    tui.press("enter").unwrap();
    env.request(&mut tui).await.answer("next answer");
    tui.wait_for(WAIT, |s| s.contains("next answer"))
        .await
        .unwrap();
    let id = env.id().await;
    let client = env.client.clone();
    let result = tokio::spawn(async move {
        client
            .request::<SessionCompact>(SessionCompactParams {
                session_id: id,
                clear: false,
            })
            .await
    });
    let summary = env.request(&mut tui).await;
    let image_message = summary.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["content"].is_array())
        .unwrap();
    env.assert_image(image_message, None);
    summary.answer("The screenshot showed red and green pixels.");
    assert!(result.await.unwrap().is_ok());
    let stored = env
        .client
        .request::<SessionMessages>(SessionRef {
            session_id: env.id().await,
        })
        .await
        .unwrap();
    assert!(
        matches!(&stored.messages[0], bone_proto::types::ChatMessage::User { images, .. } if images.len() == 1 && images[0].data.is_none())
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[tokio::test]
#[ignore = "requires a graphical clipboard; run on an isolated display"]
async fn native_screenshot_paste_reaches_provider_over_stdio() {
    let mut env = Env::new(true).await;
    let pixels = if let Some(path) = std::env::var_os("BONE_TEST_SCREENSHOT") {
        image::load_from_memory(&std::fs::read(path).unwrap())
            .unwrap()
            .into_rgba8()
    } else {
        image::RgbaImage::from_fn(1920, 1080, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        })
    };
    env.png = bone_media::from_rgba(
        pixels.width() as usize,
        pixels.height() as usize,
        pixels.as_raw(),
    )
    .unwrap()
    .bytes;
    let mut owner = arboard::Clipboard::new().unwrap();
    owner
        .set_image(arboard::ImageData {
            width: pixels.width() as usize,
            height: pixels.height() as usize,
            bytes: std::borrow::Cow::Borrowed(pixels.as_raw()),
        })
        .unwrap();
    // Real child process + NDJSON; the client filesystem does not supply a
    // server path. This covers the headless transport as well as clipboard.
    let binary =
        std::env::var_os("BONE_TEST_BINARY").unwrap_or_else(|| env!("CARGO_BIN_EXE_bone").into());
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.arg("--headless")
        .env("BONE_CONFIG_DIR", env.config.path())
        .env_remove("BONE_BASE_URL")
        .env_remove("BONE_MODEL");
    let (conn, _child) = bone_client::spawn(cmd).unwrap();
    let mut tui = Headless::start(
        conn,
        RunOptions {
            clipboard_executable: Some(binary.into()),
            cwd: env.work.path().to_string_lossy().into_owned(),
            config_dir: Some(env.config.path().to_owned()),
            resume: None,
            reload_core: false,
        },
        100,
        24,
    )
    .await
    .unwrap();
    tui.press("ctrl+v").unwrap();
    env.wait_images(&mut tui, 1).await;
    assert_eq!(tui.images()[0].name, "Screenshot");
    tui.press("enter").unwrap();
    let request = env.request(&mut tui).await;
    env.assert_image(
        request.body["messages"].as_array().unwrap().last().unwrap(),
        None,
    );
    request.answer("Clipboard screenshot received.");
    tui.wait_for(WAIT, |s| s.contains("Clipboard screenshot received."))
        .await
        .unwrap();
    for _ in 0..bone_media::MAX_IMAGES {
        env.attach(&mut tui).await;
    }
    owner.set_text("clipboard text\nsecond line").unwrap();
    tui.press("ctrl+v").unwrap();
    tui.wait_for(WAIT, |s| {
        s.contains("clipboard text") && s.contains("second line")
    })
    .await
    .unwrap();
    assert_eq!(tui.prompt_text(), "clipboard text\nsecond line");
    assert_eq!(tui.images().len(), bone_media::MAX_IMAGES);
    owner.clear().unwrap();
}

#[tokio::test]
async fn lua_can_upload_read_and_queue_an_image_only_message() {
    let mut env = Env::new(true).await;
    let core_path = env.config.path().join("core.lua");
    let mut config = std::fs::read_to_string(&core_path).unwrap();
    config.push_str(
        r#"
bone.rpc.register("queue_screenshot", function(args, ctx)
    local image = bone.attachments.upload(args.data, "Lua screenshot")
    local saved = bone.attachments.read(image.id)
    assert(saved.data == args.data)
    assert(image.data == nil)
    return bone.queue.add(ctx.session_id, "", "next", { image })
end)
"#,
    );
    std::fs::write(core_path, config).unwrap();
    env.client.request::<CoreReload>(Empty {}).await.unwrap();
    let mut tui = env.tui().await;
    tui.type_text("first message");
    tui.press("enter").unwrap();
    let first = env.request(&mut tui).await;
    let session_id = env.id().await;
    let queued = env
        .client
        .request::<LuaCall>(LuaCallParams {
            name: "queue_screenshot".into(),
            args: json!({ "data": STANDARD.encode(&env.png) }),
            session_id: Some(session_id.clone()),
            cwd: None,
        })
        .await
        .unwrap();
    assert!(queued["id"].is_number());
    let stored = env
        .client
        .request::<SessionMessages>(SessionRef { session_id })
        .await
        .unwrap();
    assert!(stored.queue[0].text.is_empty());
    assert_eq!(stored.queue[0].images[0].name, "Lua screenshot");
    assert!(stored.queue[0].images[0].data.is_none());
    first.answer("first answer");
    let second = env.request(&mut tui).await;
    env.assert_image(
        second.body["messages"].as_array().unwrap().last().unwrap(),
        None,
    );
    second.answer("Lua screenshot received.");
    tui.wait_for(WAIT, |s| s.contains("Lua screenshot received."))
        .await
        .unwrap();
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn image_send_over_local_socket_or_windows_named_pipe() {
    let mut env = Env::new(true).await;
    #[cfg(unix)]
    let path = env.work.path().join("core.sock");
    #[cfg(windows)]
    let path = std::path::PathBuf::from(format!(
        r"\\.\pipe\bone-image-tests-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let listener = bone_server::Listener::bind(&path).await.unwrap();
    let server = env.server.clone();
    let task = tokio::spawn(async move { listener.run(&server).await.unwrap() });
    let mut tui = Headless::start(
        bone_client::connect_local(&path).await.unwrap(),
        RunOptions {
            clipboard_executable: None,
            cwd: env.work.path().to_string_lossy().into_owned(),
            config_dir: Some(env.config.path().to_owned()),
            resume: None,
            reload_core: false,
        },
        100,
        24,
    )
    .await
    .unwrap();
    let (second, _) = Client::new(bone_client::connect_local(&path).await.unwrap());
    second.initialize("second-client").await.unwrap();
    env.attach(&mut tui).await;
    tui.type_text("explain this image");
    tui.press("enter").unwrap();
    let request = env.request(&mut tui).await;
    env.assert_image(
        request.body["messages"].as_array().unwrap().last().unwrap(),
        Some("explain this image"),
    );
    request.answer("Local transport received the image.");
    tui.wait_for(WAIT, |s| s.contains("Local transport received the image."))
        .await
        .unwrap();
    task.abort();
}

#[tokio::test]
async fn pending_and_failed_file_reads_preserve_the_draft() {
    let env = Env::new(true).await;
    let mut tui = env.tui().await;
    tui.type_text("keep my question");
    tui.attach_file(env.work.path().join("missing.png"));
    tui.wait_for(WAIT, |s| s.contains("cannot open"))
        .await
        .unwrap();
    assert_eq!(tui.prompt_text(), "keep my question");
    tui.attach_file(env.work.path().join("screen capture.png"));
    tui.press("enter").unwrap();
    assert_eq!(tui.prompt_text(), "keep my question");
    env.wait_images(&mut tui, 1).await;
    assert!(
        env.client
            .request::<SessionList>(Empty {})
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(tui.prompt_text(), "keep my question");
}

#[tokio::test]
async fn failed_send_restores_images_to_the_original_session_after_switching() {
    let env = Env::new(false).await;
    let mut tui = env.tui().await;
    env.attach(&mut tui).await;
    tui.type_text("question with image");
    tui.press("enter").unwrap();
    tui.press("ctrl+n").unwrap();
    tui.type_text("another session's draft");
    tui.wait_for(WAIT, |s| s.contains("does not support images"))
        .await
        .unwrap();
    assert_eq!(tui.prompt_text(), "another session's draft");
    assert!(tui.images().is_empty());
    tui.open_session(env.id().await);
    tui.settle(Duration::from_millis(100)).await;
    assert_eq!(tui.prompt_text(), "question with image");
    assert_eq!(tui.images().len(), 1);
}

#[tokio::test]
async fn history_recalls_images_and_restores_the_unsent_text_draft() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    env.attach(&mut tui).await;
    let image = tui.images()[0].clone();
    tui.type_text("sent with image");
    tui.press("enter").unwrap();
    env.request(&mut tui).await.answer("History test complete.");
    tui.wait_for(WAIT, |s| s.contains("History test complete."))
        .await
        .unwrap();
    tui.settle(Duration::from_millis(50)).await;
    tui.type_text("unfinished question");
    tui.press("home").unwrap();
    tui.press("up").unwrap();
    assert_eq!(tui.prompt_text(), "sent with image");
    assert_eq!(tui.images(), std::slice::from_ref(&image));
    tui.press("down").unwrap();
    assert_eq!(tui.prompt_text(), "unfinished question");
    assert!(tui.images().is_empty());
}

#[tokio::test]
async fn accept_images_checkbox_saves_through_core_and_reset_restores_default() {
    let env = Env::new(true).await;
    let mut tui = env.tui().await;
    tui.type_text("/config providers");
    tui.press("enter").unwrap();
    tui.wait_for(WAIT, |s| s.contains("vision-test"))
        .await
        .unwrap();
    tui.press("e").unwrap();
    tui.wait_for(WAIT, |s| s.contains("Accept images"))
        .await
        .unwrap();
    for _ in 0..10 {
        if tui.screen().contains("› Accept images") {
            break;
        }
        tui.press("down").unwrap();
    }
    assert!(tui.screen().contains("› Accept images"), "{}", tui.screen());
    tui.press("enter").unwrap();
    tui.wait_for(WAIT, |s| s.contains("saved accept images"))
        .await
        .unwrap();
    let settings = env.client.request::<SettingsGet>(Empty {}).await.unwrap();
    assert_eq!(settings["providers"]["fake"]["supports_images"], false);
    assert_eq!(
        env.client
            .request::<ModelList>(MaybeSession { session_id: None })
            .await
            .unwrap()[0]
            .supports_images,
        Some(false)
    );
    assert_eq!(
        scripting::load_with(env.config.path(), &|_| None)
            .unwrap()
            .config
            .provider
            .supports_images,
        Some(false)
    );
    env.client
        .request::<SettingsReset>(SettingPath {
            path: "providers.fake.supports_images".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        env.client
            .request::<ModelList>(MaybeSession { session_id: None })
            .await
            .unwrap()[0]
            .supports_images,
        Some(true)
    );
}

#[tokio::test]
async fn queue_image_edit_rejection_is_atomic_and_original_text_still_runs() {
    let mut env = Env::new(false).await;
    let mut tui = env.tui().await;
    let (id, first) = env.begin(&mut tui).await;
    env.attach(&mut tui).await;
    let queued = env.enqueue(&id, "original queued text", vec![]).await;
    let result = env
        .client
        .request::<QueueUpdate>(QueueUpdateParams {
            session_id: id.clone(),
            id: queued,
            text: Some("invalid replacement".into()),
            images: Some(tui.images().to_vec()),
            mode: Some(QueueMode::Steer),
        })
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("does not support images")
    );
    let state = env
        .client
        .request::<SessionMessages>(SessionRef { session_id: id })
        .await
        .unwrap();
    assert_eq!(state.queue[0].text, "original queued text");
    assert_eq!(state.queue[0].mode, QueueMode::Next);
    assert!(state.queue[0].images.is_empty());
    first.answer("first complete");
    let second = env.request(&mut tui).await;
    assert_eq!(
        second.body["messages"].as_array().unwrap().last().unwrap()["content"],
        "original queued text"
    );
    second.answer("original queued text received");
    tui.wait_for(WAIT, |s| s.contains("original queued text received"))
        .await
        .unwrap();
}

#[tokio::test]
async fn queued_image_capability_failure_is_visible_paused_and_recoverable() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    let (id, first) = env.begin(&mut tui).await;
    env.attach(&mut tui).await;
    let images = tui.images().to_vec();
    env.enqueue(&id, "queued image", images.clone()).await;
    let setting = |value| SettingSet {
        path: "providers.fake.supports_images".into(),
        value,
        session_id: None,
    };
    env.client
        .request::<SettingsSet>(setting(json!(false)))
        .await
        .unwrap();
    first.answer("first complete");
    tui.wait_for(WAIT, |s| {
        s.contains("Queue paused:") && s.contains("does not support images")
    })
    .await
    .unwrap();
    let state = env
        .client
        .request::<SessionMessages>(SessionRef {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    assert!(state.active_turn.is_none());
    assert!(state.queue_paused);
    assert_eq!(state.queue[0].images, images);
    assert!(env.requests.try_recv().is_err());
    // The paused queue survives reopening the saved session.
    let reopened = Core::from_loaded(scripting::load_with(env.config.path(), &|_| None).unwrap());
    let state = reopened
        .handle("session/messages", Some(json!({"session_id":id})))
        .await
        .unwrap();
    assert_eq!(state["queue_paused"], true);
    env.client
        .request::<SettingsSet>(setting(json!(true)))
        .await
        .unwrap();
    env.client
        .request::<QueueResume>(SessionRef { session_id: id })
        .await
        .unwrap();
    let second = env.request(&mut tui).await;
    env.assert_image(
        second.body["messages"].as_array().unwrap().last().unwrap(),
        Some("queued image"),
    );
    second.answer("queue recovered");
    tui.wait_for(WAIT, |s| s.contains("queue recovered"))
        .await
        .unwrap();
}

#[tokio::test]
async fn missing_queued_image_is_reported_and_edit_can_recover() {
    let mut env = Env::new(true).await;
    let mut tui = env.tui().await;
    let (id, first) = env.begin(&mut tui).await;
    env.attach(&mut tui).await;
    let image = tui.images()[0].clone();
    let queued = env.enqueue(&id, "queued image", vec![image.clone()]).await;
    let data_dir = scripting::load_with(env.config.path(), &|_| None)
        .unwrap()
        .config
        .data_dir;
    std::fs::remove_file(
        data_dir
            .join("attachments")
            .join(format!("{}.png", image.id)),
    )
    .unwrap();
    first.answer("first complete");
    tui.wait_for(WAIT, |s| {
        s.contains("Queue paused:") && s.contains("cannot read attachment")
    })
    .await
    .unwrap();
    let state = env
        .client
        .request::<SessionMessages>(SessionRef {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    assert!(state.queue_paused);
    assert!(state.active_turn.is_none());
    assert_eq!(state.queue[0].images, vec![image]);
    env.client
        .request::<QueueUpdate>(QueueUpdateParams {
            session_id: id.clone(),
            id: queued,
            text: Some("text recovery".into()),
            images: Some(vec![]),
            mode: None,
        })
        .await
        .unwrap();
    env.client
        .request::<QueueResume>(SessionRef { session_id: id })
        .await
        .unwrap();
    let second = env.request(&mut tui).await;
    assert_eq!(
        second.body["messages"].as_array().unwrap().last().unwrap()["content"],
        "text recovery"
    );
    second.answer("missing image recovered");
    tui.wait_for(WAIT, |s| s.contains("missing image recovered"))
        .await
        .unwrap();
}
