//! Regressions for the standard Lua UI. Use the app's real Lua API and mouse
//! hit testing; advance the Lua clock explicitly rather than sleeping for UI
//! animation or turn timing.

use super::*;

fn exec(h: &mut Harness, source: &str) {
    h.app.with_api(|lua| lua.load(source).exec()).unwrap();
}

fn text(h: &mut Harness, source: &str) -> String {
    h.app.with_api(|lua| lua.load(source).eval()).unwrap()
}

fn statusline(h: &mut Harness, id: &str) -> String {
    text(
        h,
        &format!(
            r#"
        local out = ''
        for _, span in ipairs(bone.ui.statusline({{ width = 300,
            session = {{ session_id = {id:?}, running = false }} }})) do
          out = out .. (type(span) == 'string' and span or span[1] or '')
        end
        return out
    "#
        ),
    )
}

async fn tool_fixture(detail: &str, calls: Vec<ToolCall>) -> Harness {
    let mut h = Harness::new().await;
    exec(
        &mut h,
        &format!(
            "bone.o.tool_detail = '{detail}'; bone.ui.layout = {{ 'chat' }}; \
             TEST_NOW = 1700000000000; bone.now = function() return TEST_NOW end"
        ),
    );
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<MessageCompleted>(tool_calls("s-new", calls)).await;
    h
}

fn shell(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: json!({"command": format!("echo {id}")}).to_string(),
    }
}

async fn output(h: &mut Harness, id: &str, body: &str) {
    h.emit::<ToolFinished>(ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: id.into(),
        output: body.into(),
        is_error: false,
        duration_ms: None,
    })
    .await;
}

fn click_row(h: &mut Harness, needle: &str) {
    let screen = h.screen(100, 40);
    let y = screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing clickable row {needle:?}:\n{screen}"));
    h.app.mouse("down", "left", (8, y as u16));
    h.app.mouse("up", "left", (8, y as u16));
    assert!(
        h.app.selection.is_none(),
        "tool clicks must not select text"
    );
}

#[tokio::test]
async fn standard_tool_output_click_toggles_and_global_detail_resets_override() {
    let mut h = tool_fixture("rows", vec![shell("one")]).await;
    output(
        &mut h,
        "one",
        "first\nsecond\nHIDDEN_MIDDLE\nfourth\nfifth\nsixth\nlast",
    )
    .await;
    let screen = h.screen(100, 40);
    assert!(screen.contains("click or ctrl+t"), "{screen}");
    assert!(!screen.contains("HIDDEN_MIDDLE"), "{screen}");
    click_row(&mut h, "click or ctrl+t");
    let screen = h.screen(100, 40);
    assert!(screen.contains("HIDDEN_MIDDLE"), "{screen}");
    assert!(!screen.contains("click or ctrl+t"), "{screen}");
    let y = screen
        .lines()
        .position(|l| l.contains("HIDDEN_MIDDLE"))
        .unwrap() as u16;
    h.app.mouse("down", "left", (8, y));
    h.app.mouse("drag", "left", (20, y));
    h.app.mouse("up", "left", (20, y));
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    assert_eq!(h.app.clipboard.take().as_deref(), Some("HIDDEN_MIDDLE"));
    h.app.mouse("down", "left", (8, y));
    h.app.mouse("drag", "left", (20, y));
    h.app.mouse("up", "left", (8, y));
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    click_row(&mut h, "shell echo one");
    assert!(!h.screen(100, 40).contains("HIDDEN_MIDDLE"));

    // Full mode can be locally collapsed too. Changing the global level
    // clears that local choice rather than carrying it to the next mode.
    exec(&mut h, "bone.o.tool_detail = 'full'; bone.ui.refresh()");
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    click_row(&mut h, "shell echo one");
    assert!(!h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    exec(&mut h, "bone.o.tool_detail = 'rows'; bone.ui.refresh()");
    h.screen(100, 40);
    exec(&mut h, "bone.o.tool_detail = 'full'; bone.ui.refresh()");
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
}

#[tokio::test]
async fn standard_tool_expansion_is_scoped_to_the_session() {
    let mut h = tool_fixture("rows", vec![shell("one")]).await;
    output(
        &mut h,
        "one",
        "first\nsecond\nHIDDEN_MIDDLE\nfourth\nfifth\nsixth\nlast",
    )
    .await;
    click_row(&mut h, "click or ctrl+t");
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    // Another chat can reuse the same item keys; it must not inherit this
    // chat's expansion state (nor its animation state).
    exec(
        &mut h,
        "original_session = bone.chat.session; bone.chat.session = function() return { session_id = 'another' } end; bone.ui.refresh()",
    );
    assert!(!h.screen(100, 40).contains("HIDDEN_MIDDLE"));
    exec(
        &mut h,
        "bone.chat.session = original_session; bone.ui.refresh()",
    );
    assert!(h.screen(100, 40).contains("HIDDEN_MIDDLE"));
}

#[tokio::test]
async fn standard_black_palette_is_soft_and_sidebar_matches_prompt() {
    let mut h = Harness::new().await;
    exec(
        &mut h,
        r##"
        assert(bone.hl.get('Accent').fg == '#c8dcf5')
        assert(bone.hl.get('InputBackground').bg == '#0b0b0b')
        assert(bone.hl.get('DiffAdd').bg == '#101010')
        assert(bone.hl.get('DiffDelete').bg == '#161616')
    "##,
    );
    h.input("{ctrl+o}").await;
    exec(
        &mut h,
        r##"
        assert(bone.hl.get('SessionBackground').bg == '#0b0b0b')
        assert(bone.hl.get('SessionAccent').fg == '#c8dcf5')
        bone.hl.set('Accent', { fg = '#ff0000' })
        bone.hl.set('Dim', { dim = true })
        assert(bone.hl.get('SessionAccent').fg == '#ff0000')
        assert(bone.hl.get('SessionDim').dim)
        bone.colorscheme('ansi')
        assert(bone.hl.get('SessionTab').fg == bone.hl.get('Normal').fg)
    "##,
    );
}

#[tokio::test]
async fn standard_summary_click_expands_and_collapses_the_whole_group() {
    let mut h = tool_fixture("summary", vec![shell("one"), shell("two")]).await;
    output(&mut h, "one", "OUTPUT_ONE").await;
    output(&mut h, "two", "OUTPUT_TWO").await;
    // The first draw is already completed history: no type-in delay.
    let screen = h.screen(100, 40);
    assert_eq!(
        screen.matches("Ran 2 shell commands").count(),
        1,
        "{screen}"
    );
    assert!(
        !screen.contains("OUTPUT_ONE") && !screen.contains("OUTPUT_TWO"),
        "{screen}"
    );
    click_row(&mut h, "Ran 2 shell commands");
    let screen = h.screen(100, 40);
    assert!(
        screen.contains("OUTPUT_ONE") && screen.contains("OUTPUT_TWO"),
        "{screen}"
    );
    assert!(
        screen.contains("shell echo one") && screen.contains("shell echo two"),
        "{screen}"
    );
    click_row(&mut h, "Ran 2 shell commands");
    let screen = h.screen(100, 40);
    assert!(
        !screen.contains("OUTPUT_ONE") && !screen.contains("OUTPUT_TWO"),
        "{screen}"
    );
}

#[tokio::test]
async fn standard_summary_animation_uses_clock_and_finishes_without_retyping_history() {
    let mut h = tool_fixture("summary", vec![shell("one")]).await;
    exec(
        &mut h,
        r#"
        refreshes = {}
        bone.chat.refresh_in = function(ms) refreshes[#refreshes + 1] = ms end
        function summary_text()
          local item = bone.chat.items({ kind = 'tool', first = 1 })[1]
          local rows = bone.ui.views.tool(item, { width = 100, region = 'chat' })
          local out = ''
          for _, row in ipairs(rows or {}) do
            for _, span in ipairs(row) do
              if span[2] == 'ToolArgs' then out = out .. span[1] end
            end
          end
          return out
        end
    "#,
    );
    assert_eq!(text(&mut h, "return summary_text()"), "R");
    assert_eq!(
        text(
            &mut h,
            "for _, ms in ipairs(refreshes) do if ms == 12 then return 'scheduled' end end return 'missing'"
        ),
        "scheduled"
    );
    exec(&mut h, "TEST_NOW = TEST_NOW + 24");
    assert_eq!(text(&mut h, "return summary_text()"), "Ran");
    exec(&mut h, "TEST_NOW = TEST_NOW + 1000");
    assert_eq!(text(&mut h, "return summary_text()"), "Ran 1 shell command");

    output(&mut h, "one", "done").await;
    exec(&mut h, "refreshes = {}");
    assert_eq!(text(&mut h, "return summary_text()"), "Ran 1 shell command");
    assert_eq!(text(&mut h, "return tostring(#refreshes)"), "0");

    // When the same stretch grows, retain its common prefix instead of
    // clearing the entire summary and typing it again from the first letter.
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![shell("two")]))
        .await;
    assert_eq!(text(&mut h, "return summary_text()"), "Ran 2");
    exec(&mut h, "TEST_NOW = TEST_NOW + 1000");
    assert_eq!(
        text(&mut h, "return summary_text()"),
        "Ran 2 shell commands"
    );
}

#[tokio::test]
async fn standard_turn_footer_persists_on_resume_and_not_in_statusline() {
    let (mut h, dir) = Harness::with_config("").await;
    let mut other = Harness::build(Some(dir.path().to_owned())).await;
    other.app.load_user_config();
    exec(
        &mut h,
        "TEST_NOW = 1700000000000; bone.now = function() return TEST_NOW end",
    );
    h.input("old question{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "old question"))
        .await;
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "old answer".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            reasoning_state: None,
        },
        usage: None,
    })
    .await;
    exec(&mut h, "TEST_NOW = TEST_NOW + 65000");
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    let screen = h.screen(120, 30);
    assert_eq!(screen.matches("Worked for 1m 05s").count(), 1, "{screen}");
    assert!(screen.contains("Finished at"), "{screen}");
    assert!(
        screen.find("old answer").unwrap() < screen.find("Worked for").unwrap(),
        "{screen}"
    );
    // Another TUI receives these notifications without having this chat open,
    // then saves its own chat. Neither operation may erase our saved footer.
    other
        .emit::<TurnStarted>(started("s-new", "old question"))
        .await;
    other
        .emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    exec(&mut other, "bone.api.open_session('s-other')");
    other.settle().await;
    other.emit::<TurnStarted>(started("s-other", "other")).await;
    other
        .emit::<TurnFinished>(finished("s-other", TurnOutcome::Completed))
        .await;
    drop(other);
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("state/tui/standard-turn-timing.s-new.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["turns"]["1"]["finished"], json!(1700000065000u64));
    assert!(
        !dir.path()
            .join("state/tui/personal-turn-timing.json")
            .exists()
    );
    let status = statusline(&mut h, "s-new");
    assert!(
        !status.to_lowercase().contains("worked") && !status.to_lowercase().contains("finished at"),
        "completed timing belongs in the transcript only: {status}"
    );
    drop(h);

    // A fresh app has new item keys and only the core's user/assistant
    // messages; timing must come from persisted state, not UI-only items.
    let mut resumed = Harness::build(Some(dir.path().to_owned())).await;
    resumed.app.load_user_config();
    exec(&mut resumed, "bone.api.open_session('s-new')");
    resumed.settle().await;
    let screen = resumed.screen(120, 30);
    assert!(screen.contains("old answer"), "{screen}");
    assert_eq!(screen.matches("Worked for 1m 05s").count(), 1, "{screen}");
    let status = statusline(&mut resumed, "s-new");
    assert!(
        !status.to_lowercase().contains("worked") && !status.to_lowercase().contains("finished at"),
        "{status}"
    );
}

#[tokio::test]
async fn standard_agents_spend_is_separate_and_scoped_to_root() {
    let mut h = Harness::new().await;
    // Drive the public event handlers directly to cover nested ownership,
    // usage accumulation, and agents known only from a session list.
    exec(
        &mut h,
        r#"
        local agents = require('bone.agents')
        local listed = agents.add({ session_id = 'listed', owner = { session_id = 'root' } })
        listed.tokens = 7777 -- history/list discovery is not spend from this run
    "#,
    );
    for (id, parent) in [
        ("child", "root"),
        ("nested", "child"),
        ("other", "elsewhere"),
    ] {
        h.emit::<SessionCreated>(
            serde_json::from_value(json!({
                "session_id": id, "cwd": "/work", "created_at": 0,
                "owner": { "session_id": parent, "call_id": id, "name": "worker" }
            }))
            .unwrap(),
        )
        .await;
    }
    for (id, input, output) in [
        ("child", 100, 10),
        ("child", 200, 20),
        ("nested", 300, 30),
        ("other", 9000, 900),
        ("root", 5000, 500),
    ] {
        h.emit::<MessageCompleted>(MessageCompletedParams {
            session_id: id.into(),
            turn_id: 1,
            message: ChatMessage::Assistant {
                content: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                reasoning_state: None,
            },
            usage: Some(Usage {
                input_tokens: input,
                output_tokens: output,
                ..Default::default()
            }),
        })
        .await;
    }
    exec(
        &mut h,
        r#"
        assert(require('bone.agents').spend('root') == 660)
        assert(require('bone.agents').spend('elsewhere') == 9900)
        extra, totals = {}, 0
        local request = bone.request
        bone.request = function(method, args, callback)
          if method == 'store/query' and args.sql:find("sum(input_tokens)", 1, true) then
            totals = totals + 1
            assert(not args.sql:find('sessions WHERE root', 1, true))
            assert(not args.sql:find(' OR session_id', 1, true))
          end
          if method == 'store/query' and args.sql:find("source <> 'turn'", 1, true) then
            extra[#extra + 1] = { args = args, callback = callback }
          else request(method, args, callback) end
        end
        bone._standard_status.info.root = { input = 1000, output = 100, cached = 0, curr = 900, model = 'fake' }
    "#,
    );
    for _ in 0..2 {
        h.app
            .fire("session/compacted", json!({ "session_id": "child" }));
    }
    exec(
        &mut h,
        r#"
        assert(extra[1].args.params[1] == 'child' and extra[1].args.params[2] == 0)
        extra[2].callback({ rows = {{123}} })
        extra[1].callback({ rows = {{100}} }) -- stale replies cannot reduce spend
    "#,
    );
    let status = statusline(&mut h, "root");
    assert!(
        status.contains("total 1.1k") && status.contains("agents 783"),
        "{status}"
    );
    exec(
        &mut h,
        "require('bone.agents').add({ session_id = 'resumed', owner = { session_id = 'root' } })",
    );
    h.app.fire(
        "turn/started",
        json!({ "session_id": "resumed", "turn_id": 7 }),
    );
    h.app
        .fire("session/compact_failed", json!({ "session_id": "resumed" }));
    exec(
        &mut h,
        r#"
        assert(extra[3].args.params[2] == 7) -- do not charge historical turns
        extra[3].callback({ rows = {{55}} })
        assert(require('bone.agents').spend('root') == 838)
    "#,
    );

    exec(
        &mut h,
        "bone.ui.statusline({ width = 300, session = { session_id = 'unloaded' } })",
    );
    h.settle().await;
    exec(
        &mut h,
        "assert(totals > 0, 'statusline must query its own total')",
    );
}
