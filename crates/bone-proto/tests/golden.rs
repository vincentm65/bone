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
        owner: None,
    }
}

fn queued(id: u64, mode: QueueMode, text: &str) -> QueuedMessage {
    QueuedMessage {
        id,
        text: text.into(),
        mode,
        created_at: 1_790_000_100,
        images: Vec::new(),
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
    exchange::<SessionActive>(5, Empty {}, vec![SID.into()]);
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
                    images: Vec::new(),
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
    exchange::<AttachmentUpload>(
        60,
        AttachmentUploadParams {
            data: "cG5n".into(),
            name: "Screenshot".into(),
        },
        ImageAttachment {
            id: "a".repeat(64),
            name: "Screenshot".into(),
            mime_type: "image/png".into(),
            width: 1920,
            height: 1080,
            bytes: 1024,
            data: None,
        },
    );
    exchange::<AttachmentRead>(
        61,
        AttachmentReadParams { id: "a".repeat(64) },
        AttachmentReadResult {
            data: "cG5n".into(),
        },
    );
    exchange::<TurnStart>(
        7,
        TurnStartParams {
            session_id: SID.into(),
            text: "run the tests".into(),
            images: Vec::new(),
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
        MaybeSession::default(),
        vec![
            ModelInfo {
                supports_images: None,
                name: "qwen".into(),
                model: "qwen".into(),
                kind: None,
                current: true,
                base_url: "http://localhost:8081/v1".into(),
                reasoning_effort: None,
                stream_usage: true,
                replay_reasoning: false,
                has_key: false,
                added: false,
            },
            ModelInfo {
                supports_images: None,
                name: "claude".into(),
                model: "claude-sonnet-5-5".into(),
                kind: Some("anthropic".into()),
                current: false,
                base_url: String::new(),
                reasoning_effort: Some("high".into()),
                stream_usage: true,
                replay_reasoning: false,
                has_key: true,
                added: true,
            },
        ],
    );
    exchange::<ModelComplete>(
        20,
        ModelCompleteParams {
            provider: Some("claude".into()),
            messages: vec![ChatMessage::User {
                content: "Name this session in three words.".into(),
                images: Vec::new(),
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
            images: Vec::new(),
        },
        (),
    );
    exchange::<QueueAdd>(
        30,
        QueueAddParams {
            session_id: SID.into(),
            text: "then update the README".into(),
            mode: QueueMode::Next,
            images: Vec::new(),
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
            images: None,
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
    exchange::<SessionCompact>(
        27,
        SessionCompactParams {
            session_id: SID.into(),
            clear: false,
        },
        SessionCompactedParams {
            session_id: SID.into(),
            messages: 48,
            tokens_before: 182_000,
            tokens_after: 41_000,
            reason: "manual".into(),
        },
    );
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
    exchange::<StoreQuery>(
        24,
        StoreQueryParams {
            sql:
                "SELECT model, sum(input_tokens) AS input FROM usage WHERE at >= ?1 GROUP BY model"
                    .into(),
            params: json!([1790000000]),
        },
        StoreQueryResult {
            columns: vec!["model".into(), "input".into()],
            rows: vec![vec![json!("gpt-5"), json!(120400)]],
            truncated: false,
        },
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
    let settings = json!({ "provider": "qwen", "models": { "qwen": "Qwen3-27B" }, "tui": { "tool_detail": "rows" } });
    exchange::<SettingsGet>(25, Empty {}, settings.clone());
    exchange::<SettingsSet>(
        26,
        SettingSet {
            path: "tui.tool_detail".into(),
            value: json!("rows"),
            session_id: None,
        },
        settings.clone(),
    );
    exchange::<SecretsSet>(
        28,
        SecretSet {
            provider: "deepseek".into(),
            key: Some("sk-…".into()),
        },
        vec!["deepseek".into()],
    );
    exchange::<SecretsList>(29, Empty {}, vec!["deepseek".into()]);
    exchange::<SettingsReset>(
        27,
        SettingPath {
            path: "web_search.num_results".into(),
        },
        settings,
    );
    let processes = ProcessesResult {
        version: 4,
        processes: vec![ProcessSnapshot {
            session_id: SID.into(),
            id: "shell-1".into(),
            command: "cargo test".into(),
            state: ProcessState::Running,
            running: true,
            pid: Some(1234),
            started_at_ms: 1_790_000_000_000,
            finished_at_ms: None,
            elapsed_ms: 1200,
            tail: "running".into(),
            output_bytes: 8,
            truncated: false,
            code: None,
            signal: None,
            error: None,
            terminal: true,
        }],
    };
    exchange::<ProcessesGet>(
        36,
        SessionRef {
            session_id: SID.into(),
        },
        processes.clone(),
    );
    exchange::<ProcessesList>(40, Empty {}, processes);
    exchange::<ProcessRead>(
        38,
        ProcessReadParams {
            session_id: SID.into(),
            id: "shell-1".into(),
            from: 0,
        },
        ProcessOutput {
            offset: 0,
            data: "\u{1b}[32mrunning\u{1b}[0m\r\n".into(),
            total: 19,
        },
    );
    exchange::<ProcessResize>(
        39,
        ProcessResizeParams {
            session_id: SID.into(),
            id: "shell-1".into(),
            cols: 100,
            rows: 30,
        },
        (),
    );
    exchange::<ProcessCancel>(
        37,
        ProcessRef {
            session_id: SID.into(),
            id: "shell-1".into(),
        },
        (),
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
        images: Vec::new(),
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
            context_tokens: None,
            cached_tokens: None,
        }),
    });
    event::<ToolStarted>(ToolStartedParams {
        session_id: SID.into(),
        turn_id: 1,
        call: call(),
        started_at: Some(1_767_225_600_000),
    });
    event::<ToolFinished>(ToolFinishedParams {
        session_id: SID.into(),
        turn_id: 1,
        call_id: "call_1".into(),
        output: "ok\n[exit code: 0]".into(),
        is_error: false,
        duration_ms: Some(42),
    });
    event::<SettingsChanged>(SettingsChangedParams {
        path: "tui.tool_detail".into(),
        value: json!("rows"),
        settings: json!({ "tui": { "tool_detail": "rows" } }),
    });
    event::<ToolOutput>(ToolOutputParams {
        session_id: SID.into(),
        turn_id: 1,
        call_id: "call_1".into(),
        text: "hello\n".into(),
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
            context_tokens: None,
            cached_tokens: None,
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
        error: None,
    });
    event::<TurnSteered>(TurnStartedParams {
        session_id: SID.into(),
        turn_id: 1,
        text: "also update the README".into(),
        images: Vec::new(),
    });
    event::<SessionCompacted>(SessionCompactedParams {
        session_id: SID.into(),
        messages: 12,
        tokens_before: 130_500,
        tokens_after: 98_200,
        reason: "limit".into(),
    });
    event::<SessionCreated>(SessionInfo {
        session_id: "01a0fe5e-0000-7000-8000-000000000003".into(),
        title: Some("reviewer".into()),
        owner: Some(SessionOwner {
            session_id: SID.into(),
            call_id: Some("call_7".into()),
            name: Some("reviewer".into()),
        }),
        ..info()
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
    event::<ProcessChanged>(ProcessChangedParams {
        session_id: SID.into(),
        version: 5,
        process: ProcessSnapshot {
            session_id: SID.into(),
            id: "shell-1".into(),
            command: "cargo test".into(),
            state: ProcessState::Exited,
            running: false,
            pid: Some(1234),
            started_at_ms: 1_790_000_000_000,
            finished_at_ms: Some(1_790_000_001_500),
            elapsed_ms: 1500,
            tail: "ok".into(),
            output_bytes: 4,
            truncated: false,
            code: Some(0),
            signal: None,
            error: None,
            terminal: true,
        },
        chunk: Some(ProcessChunk {
            offset: 19,
            data: "ok\r\n".into(),
        }),
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
