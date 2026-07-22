//! Regression test: interactive command (ctx.ui.key loop) must not freeze
//! when the client sends an Esc key reply. This reproduces the config-picker
//! Esc freeze in `--connect` mode.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_core::ext::{self, BootOptions};
use bone_core::llm::provider::LlmProvider;
use bone_core::rpc::{self, Hub, run_daemon, serve_connection};
use bone_core::runtime::{RuntimeCommand, RuntimeEvent, RuntimeSession};

// Minimal mock provider (not used — the command doesn't submit a turn).
struct MockProvider;
#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn name(&self) -> &str {
        "Mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }
    fn set_model(&mut self, _: String) {}
    async fn chat_stream(
        &self,
        _: Vec<bone_core::llm::ChatMessage>,
        _: Vec<bone_core::tools::ToolDefinition>,
    ) -> Result<bone_core::llm::ResponseStream, bone_core::llm::LlmError> {
        unreachable!()
    }
}

fn key_event(code: &str) -> bone_core::pane_content::KeyEvent {
    bone_core::pane_content::KeyEvent {
        code: code.to_string(),
        char: None,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

/// Drain events until the next KeyRequest, then send the reply.
async fn reply_next_key(
    events: &mut tokio::sync::broadcast::Receiver<RuntimeEvent>,
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCommand>,
    code: &str,
) {
    loop {
        match events.recv().await.unwrap() {
            RuntimeEvent::KeyRequest { id } => {
                cmd_tx
                    .send(RuntimeCommand::KeyReply {
                        id,
                        key: key_event(code),
                    })
                    .unwrap();
                return;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn interactive_command_esc_does_not_freeze() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-esc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let old_bone = std::env::var_os("BONE_DIR");
    unsafe { std::env::set_var("BONE_DIR", &config_dir) };
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();

    // A command that mimics the config picker: loops calling ctx.ui.key(),
    // renders a pane, and breaks on Esc. This is the exact pattern that froze.
    std::fs::write(
        cmd_dir.join("picker.lua"),
        r#"
local pane = require("ui.pane")
local span, wait_key, key_name = pane.span, pane.wait_key, pane.key_name

bone.command.register("picker", {
  description = "interactive picker test",
  handler = function(arg, ctx)
    local p = pane.new(ctx, { id = "interact", title = "Picker" })
    while true do
      p:set_lines({ { spans = { span("Press Esc to exit", "white") } } }, 1)
      local key = wait_key(ctx)
      if not key then break end
      local code = key_name(key)
      if code == "Esc" then break end
      if code == "Enter" then
        -- Simulate edit_text: open a sub-prompt that also loops on ctx.ui.key()
        local sub = pane.new(ctx, { id = "interact", title = "Edit" })
        while true do
          sub:set_lines({ { spans = { span("Edit - Esc cancels", "amber") } } }, 1)
          local k2 = wait_key(ctx)
          if not k2 then break end
          local c2 = key_name(k2)
          if c2 == "Esc" then break end
          if c2 == "Enter" then break end
        end
      end
    end
    pane.new(ctx, { id = "interact" }):close()
    return nil
  end,
})
"#,
    )
    .unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        true,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let extensions = booted.manager;
    let session = Arc::new(Mutex::new(RuntimeSession::new(booted.tools)));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider);
    let (hub, commands_rx) = Hub::new();
    let publisher = hub.publisher();
    tokio::spawn(run_daemon(
        publisher,
        commands_rx,
        provider,
        extensions.clone(),
        bone_core::config::store::ConfigStore::new(extensions).unwrap(),
        session,
        bone_core::tools::ApprovalMode::Safe,
        None,
        true,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_hub = hub.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = serve_connection(stream, serve_hub, Vec::new()).await;
    });

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (read_half, write_half) = tokio::io::split(stream);
    let client = rpc::RemoteClient::connect(read_half, write_half);
    let mut events = client.subscribe();
    let cmd_tx = client.command_sender();

    // Start the interactive command.
    cmd_tx
        .send(RuntimeCommand::RunCommand {
            name: "picker".into(),
            input: "".into(),
        })
        .unwrap();

    // 1. First key request from the picker loop — send Enter to open "edit".
    reply_next_key(&mut events, &cmd_tx, "Enter").await;

    // 2. Key request from the edit sub-loop — send Esc to cancel.
    reply_next_key(&mut events, &cmd_tx, "Esc").await;

    // 3. After Esc, the picker loop should resume and request another key.
    //    Send Esc to exit the picker.
    reply_next_key(&mut events, &cmd_tx, "Esc").await;

    // 4. The command should complete (nil return → CommandComplete).
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::CommandComplete { .. } => break,
                _ => {}
            }
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "interactive command did not complete after Esc — likely frozen"
    );

    // The bundled config command must use the canonical daemon schema and still
    // complete through the same remote interactive path.
    cmd_tx
        .send(RuntimeCommand::RunCommand {
            name: "config".into(),
            input: "".into(),
        })
        .unwrap();
    reply_next_key(&mut events, &cmd_tx, "Down").await;
    reply_next_key(&mut events, &cmd_tx, "Enter").await;
    reply_next_key(&mut events, &cmd_tx, "Enter").await;
    reply_next_key(&mut events, &cmd_tx, "Esc").await;
    let (schema, snapshot) = tokio::time::timeout(Duration::from_secs(10), async {
        let mut config = None;
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::ConfigSnapshot { schema, snapshot } => {
                    config = Some((schema, snapshot))
                }
                RuntimeEvent::CommandComplete { .. } => break config,
                _ => {}
            }
        }
    })
    .await
    .expect("bundled config command did not complete")
    .expect("bundled config command did not publish its canonical schema");
    let namespaces = schema
        .pages
        .iter()
        .map(|page| page.namespace.as_str())
        .collect::<Vec<_>>();
    assert!(namespaces.contains(&"general"));
    assert!(namespaces.contains(&"providers"));
    assert!(namespaces.contains(&"tools"));
    assert!(namespaces.contains(&"commands"));
    assert!(namespaces.contains(&"status"));
    assert_eq!(snapshot.values["general"]["show_reasoning"], false);

    std::fs::remove_dir_all(&config_dir).ok();
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}
