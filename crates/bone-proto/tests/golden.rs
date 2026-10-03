//! Golden wire-format tests: one file per method or event under
//! `tests/golden/`, holding an example exchange exactly as it goes over the
//! wire. Any change to the protocol's JSON shows up as a diff here.
//!
//! Regenerate after an intended change with:
//!   UPDATE_GOLDEN=1 cargo test -p bone-proto --test golden

use std::collections::BTreeSet;
use std::path::PathBuf;

use bone_proto::methods::*;
use bone_proto::types::*;
use bone_proto::{Message, Method, Notification, PROTOCOL_VERSION, RequestId, RpcError, codec};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn file_name(method: &str) -> String {
    format!("{}.json", method.replace('/', "_"))
}

/// Encode messages as NDJSON lines (the real wire form), then pretty-print
/// each line for a readable golden file.
fn wire(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(|m| serde_json::from_str(codec::encode(m).trim_end()).unwrap())
            .collect(),
    )
}

fn check(method: &str, messages: Vec<Message>) {
    let path = dir().join(file_name(method));
    let got = serde_json::to_string_pretty(&wire(&messages)).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {}; run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(
        got, want,
        "wire format of {method} changed; if intended, run with UPDATE_GOLDEN=1"
    );
    // And the golden file decodes back to the same messages.
    let lines: Vec<Value> = serde_json::from_str(&want).unwrap();
    for (line, msg) in lines.iter().zip(&messages) {
        assert_eq!(&codec::decode(&line.to_string()).unwrap(), msg, "{method}");
    }
}

fn value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

/// A request and its successful response.
fn exchange<M: Method>(id: i64, params: M::Params, result: M::Result)
where
    M::Params: PartialEq + std::fmt::Debug,
    M::Result: PartialEq + std::fmt::Debug,
{
    let msgs = vec![
        Message::Request {
            id: RequestId::Number(id),
            method: M::METHOD.into(),
            params: Some(value(&params)),
        },
        Message::Response {
            id: RequestId::Number(id),
            result: Ok(value(&result)),
        },
    ];
    // Typed values survive the trip.
    if let Message::Request {
        params: Some(p), ..
    } = &msgs[0]
    {
        assert_eq!(roundtrip::<M::Params>(p), params);
    }
    if let Message::Response { result: Ok(r), .. } = &msgs[1] {
        assert_eq!(roundtrip::<M::Result>(r), result);
    }
    check(M::METHOD, msgs);
}

fn event<N: Notification>(params: N::Params)
where
    N::Params: PartialEq + std::fmt::Debug,
{
    let v = value(&params);
    assert_eq!(roundtrip::<N::Params>(&v), params);
    check(
        N::METHOD,
        vec![Message::Notification {
            method: N::METHOD.into(),
            params: Some(v),
        }],
    );
}

fn roundtrip<T: DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap()
}

fn info() -> SessionInfo {
    SessionInfo {
        session_id: "01a0fe5e-0000-7000-8000-000000000001".into(),
        cwd: "/home/me/project".into(),
        created_at: 1_790_000_000,
        title: Some("Fix the typo in main.rs".into()),
        parent: None,
    }
}

fn queued(id: u64, mode: QueueMode, text: &str) -> QueuedMessage {
    QueuedMessage {
        id,
        text: text.into(),
        mode,
        created_at: 1_790_000_100,
    }
}

fn call() -> ToolCall {
    ToolCall {
        id: "call_1".into(),
        name: "shell".into(),
        arguments: r#"{"command":"cargo test"}"#.into(),
    }
}

const SID: &str = "01a0fe5e-0000-7000-8000-000000000001";

#[test]
fn requests() {
    exchange::<Initialize>(
        1,
        InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            client_name: "bone-tui".into(),
        },
        InitializeResult {
            protocol_version: PROTOCOL_VERSION,
            server_name: "bone".into(),
            server_version: "0.1.0".into(),
        },
    );
    exchange::<Shutdown>(2, Empty {}, ());
    exchange::<Echo>(
        3,
        EchoParams {
            text: "ping".into(),
        },
        EchoParams {
            text: "ping".into(),
        },
    );
    exchange::<SessionCreate>(
        4,
        SessionCreateParams {
            cwd: Some("/home/me/project".into()),
        },
        SessionInfo {
            title: None,
            ..info()
        },
    );
    exchange::<SessionList>(5, Empty {}, vec![info()]);
    exchange::<SessionMessages>(
        6,
        SessionRef {
            session_id: SID.into(),
        },
        SessionMessagesResult {
            info: info(),
            messages: vec![
                ChatMessage::User {
                    content: "run the tests".into(),
                },
                ChatMessage::Assistant {
                    content: String::new(),
                    reasoning: "Run cargo test.".into(),
                    tool_calls: vec![call()],
                },
                ChatMessage::Tool {
                    call_id: "call_1".into(),
                    content: "ok\n[exit code: 0]".into(),
                    is_error: false,
                },
                ChatMessage::Assistant {
                    content: "All tests pass.".into(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                },
            ],
            active_turn: None,
            queue: vec![queued(4, QueueMode::Next, "then update the README")],
            queue_paused: false,
        },
    );
    exchange::<TurnStart>(
        7,
        TurnStartParams {
            session_id: SID.into(),
            text: "run the tests".into(),
        },
        TurnStartResult { turn_id: 1 },
    );
    exchange::<TurnCancel>(
        8,
        SessionRef {
            session_id: SID.into(),
        },
        (),
    );
    exchange::<AskRespond>(
        9,
        AskRespondParams {
            ask_id: 1,
            answer: json!("allow"),
        },
        (),
    );
    exchange::<HealthCheck>(
        10,
        Empty {},
        vec![
            HealthItem {
                name: "provider".into(),
                status: HealthStatus::Ok,
                message: "deepseek-chat at https://api.deepseek.com/v1".into(),
            },
            HealthItem {
                name: "api key".into(),
                status: HealthStatus::Warn,
                message: "no API key set".into(),
            },
        ],
    );
    let plugins = || {
        vec![
            PluginInfo {
                name: "approve".into(),
                core: true,
                loaded: true,
            },
            PluginInfo {
                name: "style".into(),
                core: false,
                loaded: false,
            },
        ]
    };
    let reloaded = || ReloadResult {
        plugins: plugins(),
        warnings: vec![],
    };
    exchange::<CoreReload>(
        14,
        Empty {},
        ReloadResult {
            plugins: plugins(),
            warnings: vec!["data_dir changed; restart bone to use the new one".into()],
        },
    );
    exchange::<PluginList>(15, Empty {}, plugins());
    let approve = || PluginRef {
        name: "approve".into(),
    };
    exchange::<PluginLoad>(16, approve(), reloaded());
    exchange::<PluginUnload>(17, approve(), reloaded());
    exchange::<PluginReload>(18, approve(), reloaded());
    exchange::<ModelList>(
        19,
        Empty {},
        vec![
            ModelInfo {
                name: "qwen".into(),
                model: "qwen".into(),
                kind: None,
                current: true,
            },
            ModelInfo {
                name: "claude".into(),
                model: "claude-sonnet-5-5".into(),
                kind: Some("anthropic".into()),
                current: false,
            },
        ],
    );
    exchange::<ModelComplete>(
        20,
        ModelCompleteParams {
            provider: Some("claude".into()),
            messages: vec![ChatMessage::User {
                content: "Name this session in three words.".into(),
            }],
            tools: vec![],
            options: json!({ "max_tokens": 50 }),
            stream: true,
        },
        ModelRequest { request_id: 7 },
    );
    exchange::<ModelCancel>(21, ModelRequest { request_id: 7 }, ());
    exchange::<TurnSteer>(
        29,
        TurnSteerParams {
            session_id: SID.into(),
            text: "also update the README".into(),
        },
        (),
    );
    exchange::<QueueAdd>(
        30,
        QueueAddParams {
            session_id: SID.into(),
            text: "then update the README".into(),
            mode: QueueMode::Next,
        },
        QueueAddResult {
            id: Some(4),
            turn_id: None,
        },
    );
    let item = || QueueItemRef {
        session_id: SID.into(),
        id: 4,
    };
    exchange::<QueueRemove>(31, item(), ());
    exchange::<QueueUpdate>(
        32,
        QueueUpdateParams {
            session_id: SID.into(),
            id: 4,
            text: Some("then update the docs".into()),
            mode: Some(QueueMode::Steer),
        },
        (),
    );
    exchange::<QueueMove>(
        33,
        QueueMoveParams {
            session_id: SID.into(),
            id: 4,
            to: 0,
        },
        (),
    );
    let sref = || SessionRef {
        session_id: SID.into(),
    };
    exchange::<QueueClear>(34, sref(), ());
    exchange::<QueueResume>(35, sref(), ());
    exchange::<SessionRename>(
        26,
        SessionRenameParams {
            session_id: SID.into(),
            title: "Typo hunt".into(),
        },
        SessionInfo {
            title: Some("Typo hunt".into()),
            ..info()
        },
    );
    exchange::<SessionFork>(
        27,
        SessionForkParams {
            session_id: SID.into(),
            before_turn: Some(3),
        },
        SessionInfo {
            session_id: "01a0fe5e-0000-7000-8000-000000000002".into(),
            parent: Some(SID.into()),
            ..info()
        },
    );
    exchange::<SessionDelete>(
        28,
        SessionRef {
            session_id: SID.into(),
        },
        (),
    );
    exchange::<LuaCall>(
        23,
        LuaCallParams {
            name: "templates.expand".into(),
            args: json!({ "name": "review", "args": "src/main.rs" }),
            session_id: Some(SID.into()),
            cwd: Some("/home/me/project".into()),
        },
        json!("Review src/main.rs for bugs."),
    );
    exchange::<McpList>(
        22,
        Empty {},
        vec![
            McpServerInfo {
                name: "github".into(),
                state: "ready".into(),
                error: None,
                tools: vec!["github_search_issues".into()],
            },
            McpServerInfo {
                name: "db".into(),
                state: "failed".into(),
                error: Some("cannot run postgres-mcp: No such file or directory".into()),
                tools: vec![],
            },
        ],
    );
}

#[test]
fn events() {
    event::<Echoed>(EchoParams {
        text: "ping".into(),
    });
    event::<TurnStarted>(TurnStartedParams {
        session_id: SID.into(),
        turn_id: 1,
        text: "run the tests".into(),
    });
    event::<MessageDelta>(MessageDeltaParams {
        session_id: SID.into(),
        turn_id: 1,
        kind: DeltaKind::Text,
        text: "All tests".into(),
    });
    event::<MessageCompleted>(MessageCompletedParams {
        session_id: SID.into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: String::new(),
            reasoning: "Run cargo test.".into(),
            tool_calls: vec![call()],
        },
        usage: Some(Usage {
            input_tokens: 1200,
            output_tokens: 40,
        }),
    });
    event::<ToolStarted>(ToolStartedParams {
        session_id: SID.into(),
        turn_id: 1,
        call: call(),
    });
    event::<ToolFinished>(ToolFinishedParams {
        session_id: SID.into(),
        turn_id: 1,
        call_id: "call_1".into(),
        output: "ok\n[exit code: 0]".into(),
        is_error: false,
    });
    event::<AskRequested>(AskRequestedParams {
        ask_id: 1,
        session_id: Some(SID.into()),
        question: json!({
            "kind": "approval",
            "title": "Allow shell?",
            "tool": "shell",
            "arguments": { "command": "cargo test" },
        }),
    });
    event::<AskResolved>(AskResolvedParams {
        ask_id: 1,
        answer: json!("allow"),
    });
    event::<TurnFinished>(TurnFinishedParams {
        session_id: SID.into(),
        turn_id: 1,
        outcome: TurnOutcome::Failed {
            message: "HTTP 500: overloaded".into(),
        },
    });
    event::<ModelDeltaEvent>(ModelDeltaParams {
        request_id: 7,
        kind: DeltaKind::Text,
        text: "Fix ".into(),
    });
    event::<ModelCompleted>(ModelCompletedParams {
        request_id: 7,
        message: Some(ChatMessage::Assistant {
            content: "Fix main typo".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        }),
        usage: Some(Usage {
            input_tokens: 30,
            output_tokens: 4,
        }),
        error: None,
    });
    event::<QueueChanged>(QueueChangedParams {
        session_id: SID.into(),
        items: vec![
            queued(3, QueueMode::Steer, "also check the tests"),
            queued(4, QueueMode::Next, "then update the README"),
        ],
        paused: false,
    });
    event::<TurnSteered>(TurnStartedParams {
        session_id: SID.into(),
        turn_id: 1,
        text: "also update the README".into(),
    });
    event::<SessionDeleted>(SessionRef {
        session_id: SID.into(),
    });
    event::<SessionUpdated>(SessionUpdatedParams {
        session_id: SID.into(),
        reason: "compact".into(),
    });
    event::<CoreReloaded>(ReloadResult {
        plugins: vec![PluginInfo {
            name: "approve".into(),
            core: true,
            loaded: true,
        }],
        warnings: vec![],
    });
}

#[test]
fn errors_and_handshake_failures() {
    let mut mismatch = RpcError::new(
        RpcError::VERSION_MISMATCH,
        "protocol version mismatch: client 1, server 0",
    );
    mismatch.data = Some(json!({ "server_protocol_version": 0 }));
    check(
        "errors",
        vec![
            Message::Response {
                id: RequestId::Null,
                result: Err(RpcError::new(
                    RpcError::PARSE_ERROR,
                    "parse error: expected value at line 1 column 1",
                )),
            },
            Message::Response {
                id: RequestId::Number(1),
                result: Err(RpcError::new(
                    RpcError::NOT_INITIALIZED,
                    "first request must be initialize",
                )),
            },
            Message::Response {
                id: RequestId::Number(2),
                result: Err(mismatch),
            },
            Message::Response {
                id: RequestId::Number(3),
                result: Err(RpcError::method_not_found("nope")),
            },
            Message::Response {
                id: RequestId::Number(4),
                result: Err(RpcError::new(RpcError::BUSY, "turn 1 is still running")),
            },
        ],
    );
}

/// Every method and event has a golden file, and no file is stale.
#[test]
fn golden_files_cover_the_protocol() {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        return;
    }
    let want: BTreeSet<String> = METHODS
        .iter()
        .chain(NOTIFICATIONS)
        .map(|m| file_name(m))
        .chain(["errors.json".to_owned()])
        .collect();
    let have: BTreeSet<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(have, want);
}
