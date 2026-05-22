use bone::config::{load_providers, load_user_config, seed_providers_if_missing};
use bone::llm::providers;
use bone::ui::app::App;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Seed a default providers.yaml if this is a fresh install.
    seed_providers_if_missing();

    let cfg = load_user_config();
    let providers_config = load_providers();

    let provider = providers::create_provider_with_config(&cfg.provider, &providers_config)
        .map_err(std::io::Error::other)?;
    provider.validate().await.map_err(std::io::Error::other)?;

    let mut app = App::new(provider, providers_config)?;
    app.run().await?;
    Ok(())
}
