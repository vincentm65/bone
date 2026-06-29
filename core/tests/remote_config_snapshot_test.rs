//! Reproduce the TUI-side flow for `/config` Apply over the remote bridge:
//! an interactive key loop that ends by returning a reply-bearing
//! `config.apply` action, then the `SwitchProvider` + recv-until-snapshot
//! sequence the TUI runs (`apply_config_action` → `await_state_snapshot`).
//!
//! Both suspected freeze points are exercised in one end-to-end sequence:
//! the interactive `ctx.ui.key()` loop, and the post-complete snapshot await.
//! The test asserts the whole thing completes within a timeout.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_core::ext::{self, BootOptions};
use bone_core::llm::provider::LlmProvider;
use bone_core::rpc::{self, Hub, run_daemon, serve_connection};
use bone_core::runtime::{RuntimeCommand, RuntimeEvent, RuntimeSession};

fn key_event(code: &str) -> bone_core::pane_content::KeyEvent {
    bone_core::pane_content::KeyEvent {
        code: code.to_string(),
        char: None,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

/// Drain events until the next KeyRequest, then send the reply (mirrors the
/// TUI's `KeySink` arming a reply slot then delivering the next terminal key).
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

#[tokio::test]
async fn await_state_snapshot_unblocks_after_reply_bearing_command() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-cfgsnap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();

    // Mimics `/config` Apply: an interactive ctx.ui.key() loop (like the
    // config picker), then on Enter returns a reply-bearing `config.apply`
    // action, no output, submit=false. The daemon forces submit=false for
    // reply-bearing actions and publishes CommandComplete then a snapshot.
    std::fs::write(
        cmd_dir.join("cfgapply.lua"),
        r#"
bone.register_command("cfgapply", {
  description = "config.apply test",
  handler = function(arg, ctx)
    -- Interactive phase: read keys until Enter (the picker pattern).
    while true do
      local key = ctx.ui.key()
      if key == nil then break end
      if key.code == "Enter" then break end
    end
    return { submit = false, action = "config.apply" }
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
        extensions,
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

    // ── Mirror `run_remote_command`: consume events until CommandComplete. ──
    cmd_tx
        .send(RuntimeCommand::RunCommand {
            name: "cfgapply".into(),
            input: "".into(),
        })
        .unwrap();

    // Interactive phase: answer a couple of key requests (a stray key, then
    // Enter to exit the loop), exactly as the TUI's `drain_keys`/`KeySink` does.
    reply_next_key(&mut events, &cmd_tx, "Down").await;
    reply_next_key(&mut events, &cmd_tx, "Enter").await;

    let mut snapshots_before_complete = 0;
    loop {
        match events.recv().await.unwrap() {
            RuntimeEvent::CommandComplete { .. } => break,
            RuntimeEvent::StateSnapshot { .. } => snapshots_before_complete += 1,
            _ => {}
        }
    }

    // ── Mirror `apply_config_action(Apply)`: send SwitchProvider, then loop ──
    //    recv() until StateSnapshot (this is `await_state_snapshot`).
    cmd_tx
        .send(RuntimeCommand::SwitchProvider {
            provider_id: "mock".into(),
        })
        .unwrap();

    let got_snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap() {
                RuntimeEvent::StateSnapshot { .. } => break,
                _ => continue,
            }
        }
    })
    .await;

    std::fs::remove_dir_all(&config_dir).ok();
    assert!(
        got_snapshot.is_ok(),
        "await_state_snapshot hung after a reply-bearing command + SwitchProvider"
    );

    // The daemon publishes a snapshot right after CommandComplete (the "stale"
    // snapshot). If it landed before the break, it was consumed by the loop;
    // otherwise it is the first thing `await_state_snapshot` recv's. Either way
    // the snapshot loop unblocks — confirming there is no TUI-side freeze here.
    eprintln!(
        "snapshots seen before CommandComplete: {snapshots_before_complete}; await_state_snapshot unblocked"
    );
}
