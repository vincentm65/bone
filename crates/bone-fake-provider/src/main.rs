//! `bone-fake-provider [--port 4100] [--backend URL] [--model NAME]`
//!
//! Serves the fake APIs, answering with the model behind `--backend` (an
//! OpenAI-compatible `/chat/completions` server, default tabbyAPI on
//! localhost:8081). Point a bone provider at `http://127.0.0.1:4100/v1`, or
//! put a flavor or fault before `/v1` (see the crate docs).

use bone_fake_provider::{Backend, FakeServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = 4100;
    let mut backend = "http://localhost:8081/v1".to_owned();
    let mut model = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--port" => port = value()?.parse()?,
            "--backend" => backend = value()?,
            "--model" => model = Some(value()?),
            "-h" | "--help" => {
                println!("bone-fake-provider [--port 4100] [--backend URL] [--model NAME]");
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let model = match model {
        Some(m) => m,
        None => first_model(&backend).await?,
    };
    let server = FakeServer::start(Backend::live(&backend, &model), port).await?;
    eprintln!("fake provider on {}/v1 -> {backend} ({model})", server.url);
    std::future::pending::<()>().await;
    Ok(())
}

/// The backend's first listed model that is not a loader placeholder.
async fn first_model(backend: &str) -> Result<String, Box<dyn std::error::Error>> {
    let v: serde_json::Value = reqwest::get(format!("{}/models", backend.trim_end_matches('/')))
        .await?
        .json()
        .await?;
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str())
        .find(|id| *id != "exllamav3")
        .map(str::to_owned)
        .ok_or_else(|| "the backend lists no models; pass --model".into())
}
