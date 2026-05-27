use bone::config::{
    load_providers, load_user_config, seed_command_policy_if_missing, seed_providers_if_missing,
};
use bone::llm::providers;
use bone::ui::app::App;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    seed_providers_if_missing();
    seed_command_policy_if_missing();

    let cfg = load_user_config();
    let providers_config = load_providers();

    let provider_id = cfg.provider.as_str();
    let provider_id = if providers_config.last_provider.is_empty() {
        provider_id
    } else {
        &providers_config.last_provider
    };

    let provider = providers::create_provider_with_config(provider_id, &providers_config)
        .map_err(std::io::Error::other)?;
    provider.validate().await.map_err(std::io::Error::other)?;

    let mut app = App::new(provider, providers_config, cfg)?;
    app.run().await?;
    Ok(())
}
