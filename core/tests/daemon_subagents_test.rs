use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bone_core::config::settings::Settings;
use bone_core::ext::{self, BootOptions};
use bone_core::llm::provider::LlmProvider;
use bone_core::llm::{ChatEvent, ChatMessage, LlmError, ResponseStream};
use bone_core::rpc::{Hub, run_daemon};
use bone_core::runtime::{RuntimeCommand, RuntimeEvent, RuntimeSession};
use bone_core::tools::ApprovalMode;
use bone_protocol::SubagentDefinition;
use futures_util::StreamExt;

struct EmptyProvider;

#[async_trait]
impl LlmProvider for EmptyProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn name(&self) -> &str {
        "Mock"
    }

    fn model(&self) -> &str {
        "mock-1"
    }

    fn set_model(&mut self, _model: String) {}

    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<bone_core::tools::ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        Ok(futures_util::stream::empty::<Result<ChatEvent, LlmError>>().boxed())
    }
}

async fn next_subagents(
    events: &mut tokio::sync::broadcast::Receiver<RuntimeEvent>,
) -> Vec<SubagentDefinition> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RuntimeEvent::FrontendState { subagents, .. } = events.recv().await.unwrap() {
                break subagents;
            }
        }
    })
    .await
    .expect("daemon did not publish updated frontend state")
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_subagent_crud_persists_and_updates_frontend_state() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-daemon-subagents-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"bone.subagent.register({
    name = "reviewer",
    description = "Reviews Lua changes",
    approval = "safe",
})
"#,
    )
    .unwrap();
    unsafe { std::env::set_var("BONE_DIR", &config_dir) };

    let settings = Arc::new(Mutex::new(Settings::load().unwrap().unwrap()));
    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = ext::boot_with_tools_shared(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        BootOptions::default(),
        "mock-1",
        "mock",
        settings,
    );
    let session = Arc::new(Mutex::new(RuntimeSession::new(booted.tools)));
    let (hub, commands) = Hub::new();
    let mut events = hub.subscribe();
    let daemon = tokio::spawn(run_daemon(
        hub.publisher(),
        commands,
        Arc::new(EmptyProvider),
        booted.manager,
        session,
        ApprovalMode::Safe,
        None,
        false,
    ));

    hub.command_sender()
        .send(RuntimeCommand::SetSubagentEnabled {
            name: "reviewer".into(),
            enabled: false,
        })
        .unwrap();
    let agents = next_subagents(&mut events).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "reviewer");
    assert_eq!(agents[0].source, "config");
    assert!(!agents[0].enabled);
    let saved = Settings::load().unwrap().unwrap();
    assert!(!saved.resolved().subagents["reviewer"].enabled);

    hub.command_sender()
        .send(RuntimeCommand::UpsertSubagent {
            agent: SubagentDefinition {
                name: "reviewer".into(),
                description: "Reviews changes".into(),
                system_prompt: Some("Find concrete regressions.".into()),
                approval: "danger".into(),
                timeout_ms: Some(30_000),
                enabled: true,
                source: "lua".into(),
                ..Default::default()
            },
        })
        .unwrap();
    let agents = next_subagents(&mut events).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "reviewer");
    assert_eq!(agents[0].source, "config");
    assert_eq!(agents[0].approval, "danger");

    hub.command_sender()
        .send(RuntimeCommand::SetSubagentEnabled {
            name: "reviewer".into(),
            enabled: false,
        })
        .unwrap();
    let agents = next_subagents(&mut events).await;
    assert_eq!(agents.len(), 1);
    assert!(!agents[0].enabled);
    let saved = Settings::load().unwrap().unwrap();
    assert!(!saved.resolved().subagents["reviewer"].enabled);

    hub.command_sender()
        .send(RuntimeCommand::DeleteSubagent {
            name: "reviewer".into(),
        })
        .unwrap();
    let agents = next_subagents(&mut events).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "reviewer");
    assert_eq!(agents[0].source, "lua");
    assert!(agents[0].enabled);
    assert!(
        Settings::load()
            .unwrap()
            .unwrap()
            .resolved()
            .subagents
            .is_empty()
    );

    daemon.abort();
    std::fs::remove_dir_all(config_dir).unwrap();
}
