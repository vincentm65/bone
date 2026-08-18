use crate::config::{ProviderCredential, ProviderEntry, ProvidersConfig};

#[test]
fn invalid_provider_updates_preserve_existing_document() {
    let _guard = crate::util::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.yaml");
    let original = b"version: 1\nactive: ''\nproviders: {}\n";
    std::fs::write(&path, original).unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");

    let result = std::panic::catch_unwind(|| {
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };
        for (max_concurrency, handler, fast_mode, expected_error) in [
            (
                Some(0),
                "openai",
                false,
                "max_concurrency must be at least 1",
            ),
            (None, "openai", true, "fast_mode is only supported"),
        ] {
            let mut config = ProvidersConfig::default();
            config.providers.insert(
                "test".into(),
                ProviderEntry {
                    label: "Test".into(),
                    base_url: String::new(),
                    model: String::new(),
                    api_key: ProviderCredential::default(),
                    endpoint: "/chat/completions".into(),
                    handler: handler.into(),
                    context_window_tokens: None,
                    max_concurrency,
                    reasoning_effort: "ultra".into(),
                    fast_mode,
                    supports_prompt_cache_key: false,
                    stream_usage: "auto".into(),
                },
            );

            let error = super::persist_providers(&config).unwrap_err();
            assert!(error.contains(expected_error), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    });

    match old_bone_dir {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
