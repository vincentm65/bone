//! A minimal line-mode client: the smallest complete example of driving the
//! bone API (sessions, streaming, questions from Lua) from Rust.
//!
//! ```sh
//! BONE_BASE_URL=http://localhost:8081/v1 BONE_MODEL=my-model \
//!   cargo run -p bone-server --example chat -- "list the files here"
//! ```
//!
//! Runs the core in-process, configured like `bone` (~/.bone/core.lua and
//! `BONE_*` overrides; see `bone --help`). With `BONE_CONNECT=<socket>` it instead talks to a running
//! `bone --headless --listen`. Each argument is sent as one turn in the same
//! session; with no arguments, prompts are read from stdin.

use std::io::{BufRead, Write};
use std::sync::Arc;

use bone_client::{Client, connect_unix};
use bone_core::Core;
use bone_core::scripting;
use bone_proto::methods::*;
use bone_proto::types::{DeltaKind, TurnOutcome};
use bone_server::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = match std::env::var("BONE_CONNECT").ok().filter(|v| !v.is_empty()) {
        Some(sock) => connect_unix(sock).await?,
        None => {
            let server = Server::new(Arc::new(Core::from_loaded(scripting::load(
                &bone_lua::config_dir(),
            )?)));
            server.connect_in_process()
        }
    };
    let (client, mut events) = Client::new(conn);
    client.initialize("chat-example").await?;
    let session = client
        .request::<SessionCreate>(SessionCreateParams {
            // A socket server's own cwd is not ours.
            cwd: Some(std::env::current_dir()?.to_string_lossy().into_owned()),
        })
        .await?;
    eprintln!("session {} in {}", session.session_id, session.cwd);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let prompts: Box<dyn Iterator<Item = String>> = if args.is_empty() {
        Box::new(
            std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
                .into_iter(),
        )
    } else {
        Box::new(args.into_iter())
    };

    for text in prompts {
        client
            .request::<TurnStart>(TurnStartParams {
                session_id: session.session_id.clone(),
                text,
            })
            .await?;
        let mut in_reasoning = false;
        loop {
            let Some(e) = events.recv().await else {
                return Ok(());
            };
            if let Some(Ok(d)) = e.parse::<MessageDelta>() {
                let reasoning = d.kind == DeltaKind::Reasoning;
                if reasoning != in_reasoning {
                    print!("{}", if reasoning { "\x1b[2m" } else { "\x1b[0m\n" });
                    in_reasoning = reasoning;
                }
                print!("{}", d.text);
                std::io::stdout().flush()?;
            } else if let Some(Ok(t)) = e.parse::<ToolStarted>() {
                println!(
                    "\x1b[0m\n\x1b[36m▶ {} {}\x1b[0m",
                    t.call.name, t.call.arguments
                );
                in_reasoning = false;
            } else if let Some(Ok(t)) = e.parse::<ToolFinished>() {
                let color = if t.is_error { "31" } else { "32" };
                let preview: String = t.output.lines().take(8).collect::<Vec<_>>().join("\n  ");
                println!("\x1b[{color}m  {preview}\x1b[0m");
            } else if let Some(Ok(q)) = e.parse::<AskRequested>() {
                // Questions from core Lua (e.g. the approve plugin).
                let approval = q.question["kind"] == "approval";
                match q.question["title"].as_str() {
                    Some(title) if approval => {
                        eprint!("{title} {} [y/N/a] ", q.question["arguments"])
                    }
                    _ => eprint!("{} > ", q.question),
                }
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                let answer = match (approval, line.trim()) {
                    (true, "y" | "Y") => serde_json::json!("allow"),
                    (true, "a" | "A") => serde_json::json!("always"),
                    (true, _) => serde_json::json!("deny"),
                    (false, text) => serde_json::json!(text),
                };
                client
                    .request::<AskRespond>(AskRespondParams {
                        ask_id: q.ask_id,
                        answer,
                    })
                    .await?;
            } else if let Some(Ok(m)) = e.parse::<MessageCompleted>() {
                if let Some(u) = m.usage {
                    eprintln!(
                        "\x1b[0m\n\x1b[2m[{} in / {} out]\x1b[0m",
                        u.input_tokens, u.output_tokens
                    );
                }
            } else if let Some(Ok(f)) = e.parse::<TurnFinished>() {
                match f.outcome {
                    TurnOutcome::Completed => println!(),
                    other => println!("\n[{other:?}]"),
                }
                break;
            }
        }
    }
    Ok(())
}
