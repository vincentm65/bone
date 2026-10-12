//! bone's built-in providers against the fake APIs: whole sessions, every
//! format, every fault. Scripted tests run offline; `live_*` tests (ignored)
//! answer with the model on `BONE_FAKE_BACKEND` (default tabbyAPI on
//! localhost:8081):
//!
//! ```sh
//! cargo test -p bone-fake-provider --test matrix -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use bone_client::Client;
use bone_core::Core;
use bone_core::config::{CoreConfig, ProviderConfig};
use bone_fake_provider::{Backend, FakeServer, Reply};
use bone_proto::methods::*;
use bone_proto::types::{ChatMessage, ProviderApi, TurnOutcome};
use bone_server::Server;
use serde_json::json;

/// One way of reaching the fake server.
#[derive(Clone, Copy, Debug)]
struct Route {
    api: ProviderApi,
    /// Path before `/v1`: a flavor and/or faults.
    prefix: &'static str,
    replay_reasoning: bool,
}

const COMPLETIONS: Route = Route {
    api: ProviderApi::Completions,
    prefix: "",
    replay_reasoning: false,
};
const RESPONSES: Route = Route {
    api: ProviderApi::Responses,
    prefix: "",
    replay_reasoning: false,
};
const ANTHROPIC: Route = Route {
    api: ProviderApi::Anthropic,
    prefix: "",
    replay_reasoning: false,
};

/// Every format and dialect bone should handle.
const ROUTES: &[Route] = &[
    COMPLETIONS,
    Route {
        prefix: "/flavor/deepseek",
        ..COMPLETIONS
    },
    Route {
        prefix: "/flavor/qwen",
        replay_reasoning: true,
        ..COMPLETIONS
    },
    Route {
        prefix: "/flavor/openrouter",
        ..COMPLETIONS
    },
    RESPONSES,
    ANTHROPIC,
];

/// Long enough that the system prompt and tools pass Anthropic's minimum
/// cacheable prefix.
fn system_prompt() -> String {
    let rule = "Answer briefly. Use the shell tool to inspect files, and read_file to read them. ";
    format!("You are a test assistant. {}", rule.repeat(60))
}

struct Turn {
    outcome: TurnOutcome,
    text: String,
    reasoning: String,
    tools: usize,
}

struct Session {
    turns: Vec<Turn>,
    messages: Vec<ChatMessage>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

/// Run `prompts` as turns of one session through `route`. `/compact`
/// compacts the session (keeping the last turn) instead of a turn.
async fn session(server: &FakeServer, route: Route, prompts: &[&str]) -> Session {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("a.txt"), "one\ntwo\n").unwrap();
    let core = Core::new(CoreConfig {
        provider: ProviderConfig {
            base_url: format!("{}{}/v1", server.url, route.prefix),
            model: "fake-model".into(),
            api_key: Some("sk-fake".into()),
            replay_reasoning: route.replay_reasoning,
            api: route.api,
            ..Default::default()
        },
        system_prompt: Some(system_prompt()),
        data_dir: data.path().to_owned(),
        parallel_tools: true,
        compact: bone_core::config::CompactConfig {
            keep: 1,
            ..Default::default()
        },
    });
    let server = Server::new(Arc::new(core));
    let (client, mut events) = Client::new(server.connect_in_process());
    client.initialize("matrix").await.unwrap();
    let info = client
        .request::<SessionCreate>(SessionCreateParams {
            cwd: Some(work.path().to_string_lossy().into()),
        })
        .await
        .unwrap();
    let mut turns = Vec::new();
    for prompt in prompts {
        if *prompt == "/compact" {
            client
                .request::<SessionCompact>(SessionCompactParams {
                    session_id: info.session_id.clone(),
                    clear: false,
                })
                .await
                .expect("compaction failed");
            continue;
        }
        client
            .request::<TurnStart>(TurnStartParams {
                session_id: info.session_id.clone(),
                text: (*prompt).into(),
                images: Vec::new(),
            })
            .await
            .unwrap();
        let mut turn = Turn {
            outcome: TurnOutcome::Cancelled,
            text: String::new(),
            reasoning: String::new(),
            tools: 0,
        };
        loop {
            let e = tokio::time::timeout(Duration::from_secs(300), events.recv())
                .await
                .expect("a turn took over 5 minutes")
                .expect("events ended");
            if let Some(Ok(m)) = e.parse::<MessageCompleted>()
                && let ChatMessage::Assistant {
                    content, reasoning, ..
                } = m.message
            {
                turn.text.push_str(&content);
                turn.reasoning.push_str(&reasoning);
            } else if e.parse::<ToolFinished>().is_some() {
                turn.tools += 1;
            } else if let Some(Ok(f)) = e.parse::<TurnFinished>() {
                turn.outcome = f.outcome;
                break;
            }
        }
        turns.push(turn);
    }
    let messages = client
        .request::<SessionMessages>(SessionRef {
            session_id: info.session_id,
        })
        .await
        .unwrap()
        .messages;
    Session {
        turns,
        messages,
        _dirs: (data, work),
    }
}

/// Requests the fake server turned down (not counting injected faults).
fn rejected(server: &FakeServer) -> Vec<String> {
    server
        .log()
        .into_iter()
        .filter(|l| {
            l.status >= 400
                && !l
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("fake: injected")
        })
        .map(|l| format!("{} {}: {}", l.status, l.path, l.error.unwrap_or_default()))
        .collect()
}

fn script() -> Vec<Reply> {
    vec![
        Reply::call("shell", json!({ "command": "cat a.txt" })).thinking("I should read the file."),
        Reply::text("It has two lines.").thinking("Two lines: one, two."),
        Reply::call("shell", json!({ "command": "echo three >> a.txt" }))
            .thinking("Append with echo."),
        Reply::call("read_file", json!({ "path": "a.txt" })).thinking("Now read it back."),
        Reply::text("Done: three lines.").thinking("It worked."),
    ]
}

const PROMPTS: &[&str] = &[
    "How many lines does a.txt have?",
    "Append 'three' and read it back.",
];

#[tokio::test]
async fn every_route_runs_a_multi_turn_tool_session() {
    for route in ROUTES {
        let server = FakeServer::start(Backend::scripted(script()), 0)
            .await
            .unwrap();
        let s = session(&server, *route, PROMPTS).await;
        let what = format!("{route:?}");
        assert_eq!(rejected(&server), Vec::<String>::new(), "{what}");
        for t in &s.turns {
            assert_eq!(t.outcome, TurnOutcome::Completed, "{what}");
        }
        assert_eq!(s.turns[0].tools, 1, "{what}");
        assert_eq!(s.turns[1].tools, 2, "{what}");
        assert!(s.turns[1].text.contains("three lines"), "{what}");
        // Reasoning shows wherever the dialect streams it (not plain
        // OpenAI Chat Completions).
        let streams_reasoning =
            !matches!((route.api, route.prefix), (ProviderApi::Completions, ""));
        assert_eq!(
            !s.turns[0].reasoning.is_empty(),
            streams_reasoning,
            "{what}"
        );
    }
}

#[tokio::test]
async fn responses_sends_sealed_reasoning_back() {
    let server = FakeServer::start(Backend::scripted(script()), 0)
        .await
        .unwrap();
    let s = session(&server, RESPONSES, PROMPTS).await;
    assert_eq!(rejected(&server), Vec::<String>::new());
    let replayed: Vec<usize> = server.log().iter().map(|l| l.reasoning_replayed).collect();
    // Each call sends back every earlier step's reasoning.
    assert_eq!(replayed, [0, 1, 2, 3, 4]);
    assert!(s.messages.iter().any(|m| matches!(
        m,
        ChatMessage::Assistant {
            reasoning_state: Some(_),
            ..
        }
    )));
}

#[tokio::test]
async fn anthropic_reads_the_whole_previous_prompt_from_cache() {
    let server = FakeServer::start(Backend::scripted(script()), 0)
        .await
        .unwrap();
    let s = session(&server, ANTHROPIC, PROMPTS).await;
    assert_eq!(rejected(&server), Vec::<String>::new());
    assert!(s.turns.iter().all(|t| t.outcome == TurnOutcome::Completed));
    let log = server.log();
    assert!(
        log[0].prompt_tokens >= 1024,
        "the test prompt must be cacheable"
    );
    assert_eq!(
        (log[0].cache_read, log[0].cache_write),
        (0, log[0].prompt_tokens)
    );
    // Each call reads everything the one before it sent, and writes only
    // what was added since.
    for pair in log.windows(2) {
        let (before, now) = (&pair[0], &pair[1]);
        assert_eq!(now.cache_read, before.prompt_tokens, "{}", now.path);
        assert_eq!(now.cache_write, now.prompt_tokens - before.prompt_tokens);
    }
}

#[tokio::test]
async fn anthropic_sends_thinking_back_until_history_changes() {
    let mut replies = script();
    replies.push(Reply::text(
        "Summary: a.txt was read, then 'three' appended.",
    ));
    replies.push(Reply::call("shell", json!({ "command": "wc -l a.txt" })).thinking("Count."));
    replies.push(Reply::text("Three lines.").thinking("Done."));
    let server = FakeServer::start(Backend::scripted(replies), 0)
        .await
        .unwrap();
    let mut prompts = PROMPTS.to_vec();
    prompts.extend(["/compact", "Count the lines again."]);
    let s = session(&server, ANTHROPIC, &prompts).await;
    // Nothing rejected: thinking from before the compaction (whose history
    // changed) was left out, everything else went back.
    assert_eq!(rejected(&server), Vec::<String>::new());
    assert!(s.turns.iter().all(|t| t.outcome == TurnOutcome::Completed));
    let replayed: Vec<usize> = server.log().iter().map(|l| l.reasoning_replayed).collect();
    // 5 calls, the compaction's summary call, then 2 calls after it: the
    // kept turn's thinking is stale, only the new turn's goes back.
    assert_eq!(replayed, [0, 1, 2, 3, 4, 0, 0, 1]);
}

#[tokio::test]
async fn responses_never_sends_back_unsealed_reasoning() {
    let server = FakeServer::start(Backend::scripted(script()), 0)
        .await
        .unwrap();
    let route = Route {
        prefix: "/flavor/unsealed",
        ..RESPONSES
    };
    let s = session(&server, route, PROMPTS).await;
    assert_eq!(rejected(&server), Vec::<String>::new());
    assert!(s.turns.iter().all(|t| t.outcome == TurnOutcome::Completed));
    assert!(!s.turns[0].reasoning.is_empty(), "summaries still show");
}

/// A fault on the second call (mid-turn, after a tool ran).
async fn faulted(route: Route, fault: &str) -> (Session, FakeServer) {
    let server = FakeServer::start(Backend::scripted(script()), 0)
        .await
        .unwrap();
    let prefix: &'static str =
        Box::leak(format!("{}/fault/{fault}@2", route.prefix).into_boxed_str());
    let s = session(&server, Route { prefix, ..route }, &PROMPTS[..1]).await;
    (s, server)
}

fn failed_with(t: &Turn, what: &str) {
    match &t.outcome {
        TurnOutcome::Failed { message } => {
            assert!(
                message.contains(what),
                "{message:?} should mention {what:?}"
            )
        }
        other => panic!("expected a failure mentioning {what:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn faults_end_the_turn_with_an_error_never_silently() {
    for route in [COMPLETIONS, RESPONSES, ANTHROPIC] {
        let (s, _) = faulted(route, "cut").await;
        failed_with(&s.turns[0], "ended");
        let (s, _) = faulted(route, "incomplete").await;
        failed_with(&s.turns[0], "incomplete");
        let (s, _) = faulted(route, "error_body").await;
        failed_with(&s.turns[0], "upstream failed");
        let (s, _) = faulted(route, "slow").await;
        assert_eq!(s.turns[0].outcome, TurnOutcome::Completed, "{route:?} slow");
        // A reply with nothing in it that finished properly is not an error.
        let (s, _) = faulted(route, "empty").await;
        assert_eq!(
            s.turns[0].outcome,
            TurnOutcome::Completed,
            "{route:?} empty"
        );
    }
}

#[tokio::test]
async fn transient_http_errors_are_retried() {
    for route in [COMPLETIONS, RESPONSES, ANTHROPIC] {
        for fault in ["http429", "http500", "http529"] {
            let (s, server) = faulted(route, fault).await;
            assert_eq!(
                s.turns[0].outcome,
                TurnOutcome::Completed,
                "{route:?} {fault}"
            );
            assert_eq!(server.log().len(), 3, "{route:?} {fault}: one retry");
        }
    }
}

// ---- live: the local model behind the fake APIs ---------------------------

async fn live_server() -> FakeServer {
    let backend = std::env::var("BONE_FAKE_BACKEND").unwrap_or("http://localhost:8081/v1".into());
    let models: serde_json::Value = reqwest::get(format!("{backend}/models"))
        .await
        .expect("the live backend is not running")
        .json()
        .await
        .unwrap();
    let model = std::env::var("BONE_FAKE_MODEL").ok().unwrap_or_else(|| {
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .find(|id| *id != "exllamav3")
            .unwrap()
            .to_owned()
    });
    FakeServer::start(Backend::live(&backend, &model), 0)
        .await
        .unwrap()
}

const LIVE_PROMPTS: &[&str] = &[
    "List the files here with the shell tool.",
    "Read a.txt and tell me how many lines it has.",
    "Append a line 'three' to a.txt with the shell, then read it back.",
    "Count the words in a.txt with wc, then summarize everything you did.",
];

#[tokio::test]
#[ignore = "needs the live backend"]
async fn live_every_route() {
    let mut failures = Vec::new();
    for route in ROUTES {
        let server = live_server().await;
        let s = session(&server, *route, LIVE_PROMPTS).await;
        let mut problems = rejected(&server);
        for (i, t) in s.turns.iter().enumerate() {
            if t.outcome != TurnOutcome::Completed {
                problems.push(format!("turn {i}: {:?}", t.outcome));
            } else if t.text.trim().is_empty() {
                problems.push(format!("turn {i}: no answer"));
            }
        }
        if s.turns.iter().map(|t| t.tools).sum::<usize>() < 3 {
            problems.push("fewer than 3 tool calls".into());
        }
        if route.api != ProviderApi::Completions
            && server.log().iter().all(|l| l.reasoning_replayed == 0)
        {
            problems.push("no reasoning was sent back".into());
        }
        if route.api == ProviderApi::Anthropic {
            let log = server.log();
            let misses = log
                .windows(2)
                .filter(|w| w[1].cache_read < w[0].prompt_tokens)
                .count();
            if misses > 0 {
                problems.push(format!(
                    "{misses} calls missed the previous prompt in cache"
                ));
            }
        }
        eprintln!(
            "{route:?}: {}",
            if problems.is_empty() {
                "ok".into()
            } else {
                problems.join("; ")
            }
        );
        if !problems.is_empty() {
            failures.push(format!("{route:?}: {}", problems.join("; ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
