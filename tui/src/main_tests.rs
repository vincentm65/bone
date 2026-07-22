use super::actor_provider_config;
use super::cli::{approval_mode, has_flag, parse_provider_model};
use bone::tools::ApprovalMode;
use bone_protocol::ProviderUpdate;

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn provider_update(id: &str, model: &str) -> ProviderUpdate {
    ProviderUpdate {
        id: id.into(),
        label: id.into(),
        base_url: "http://localhost:11434/v1".into(),
        model: model.into(),
        endpoint: "/chat/completions".into(),
        handler: "openai".into(),
        context_window_tokens: None,
        reasoning_effort: String::new(),
        api_key: None,
    }
}

#[test]
fn parse_provider_model_extracts_both() {
    let (p, m) = parse_provider_model(&args(&[
        "--listen",
        "127.0.0.1:7878",
        "--provider",
        "codex",
        "--model",
        "gpt-5.5",
    ]));
    assert_eq!(p.as_deref(), Some("codex"));
    assert_eq!(m.as_deref(), Some("gpt-5.5"));
}

#[test]
fn parse_provider_model_ignores_unknown_and_missing() {
    // Unlike `parse_cli_options`, unknown flags (e.g. `--listen`) are ignored
    // rather than rejected, and absent flags yield `None`.
    let (p, m) = parse_provider_model(&args(&["--listen", "x", "--verbose"]));
    assert!(p.is_none());
    assert!(m.is_none());
}

#[test]
fn has_flag_detects_presence() {
    assert!(has_flag(
        &args(&["--shutdown-on-stdin-eof"]),
        "--shutdown-on-stdin-eof"
    ));
    assert!(!has_flag(
        &args(&["--listen", "x"]),
        "--shutdown-on-stdin-eof"
    ));
}

#[test]
fn approval_mode_uses_canonical_setting() {
    assert_eq!(approval_mode("danger"), ApprovalMode::Danger);
    assert_eq!(approval_mode("safe"), ApprovalMode::Safe);
    assert_eq!(approval_mode("invalid"), ApprovalMode::Safe);
}

#[test]
fn actor_provider_config_reads_current_store_and_applies_cli_overrides() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let old_bone = std::env::var_os("BONE_DIR");
    let dir = std::env::temp_dir().join(format!(
        "bone-actor-provider-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    unsafe { std::env::set_var("BONE_DIR", &dir) };

    let store =
        bone::config::store::ConfigStore::new(bone::ext::ExtensionManager::unloaded()).unwrap();
    let mut revision = store.snapshot().revision;
    store
        .upsert_provider(provider_update("first", "first-model"), revision)
        .unwrap();
    revision = store.snapshot().revision;
    store.set_active_provider("first", revision).unwrap();

    let (provider, config) = actor_provider_config(&store, None, None).unwrap();
    assert_eq!(provider, "first");
    assert_eq!(config.providers["first"].model, "first-model");

    revision = store.snapshot().revision;
    store
        .upsert_provider(provider_update("second", "second-model"), revision)
        .unwrap();
    revision = store.snapshot().revision;
    store.set_active_provider("second", revision).unwrap();

    let (provider, config) = actor_provider_config(&store, None, None).unwrap();
    assert_eq!(provider, "second");
    assert_eq!(config.providers["second"].model, "second-model");

    let (provider, config) =
        actor_provider_config(&store, Some("first"), Some("cli-model")).unwrap();
    assert_eq!(provider, "first");
    assert_eq!(config.providers["first"].model, "cli-model");

    std::fs::remove_dir_all(dir).ok();
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}
