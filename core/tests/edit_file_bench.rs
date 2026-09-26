//! Benchmark: anchor-based `edit_file` on simulated multi-step edit tasks.
//!
//! Run with:
//! `cargo test -p bone-core --release --test edit_file_bench -- --ignored --nocapture`
//!
//! Env knobs: `BENCH_FILES` (default 20), `BENCH_SEEDS` (default 5).
//!
//! The tool replays a seeded plan (targets, replacement text, external edits,
//! call batching and ordering). A line-id oracle computes the intended file
//! after every call; a call that "succeeds" but leaves different content
//! counts as silent corruption. The simulated model:
//! - reads the whole file or +/-15-line windows around its targets;
//! - keeps using anchors from its first read (stale after its own and
//!   external edits);
//! - optionally miscopies each character copied from the file with probability
//!   `eps` (anchors for edit_file);
//! - on failure retries up to 3 attempts, re-reading with a wider window,
//!   except that it reuses fresh anchors printed in the error when present.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bone_core::tools::edit_file::{EditFileTool, line_hash, render_line};
use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::types::{Tool, ToolExecutionContext};
use serde_json::{Map, Value, json};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64) / ((1u64 << 53) as f64) < p
    }
}

#[derive(Clone)]
struct Line {
    id: u64,
    text: String,
}

#[derive(Clone)]
struct Doc {
    lines: Vec<Line>,
}

impl Doc {
    fn new(text: &str) -> Self {
        Doc {
            lines: text
                .lines()
                .enumerate()
                .map(|(i, t)| Line {
                    id: i as u64 + 1,
                    text: t.to_string(),
                })
                .collect(),
        }
    }
    fn text(&self) -> String {
        let mut s = String::new();
        for line in &self.lines {
            s.push_str(&line.text);
            s.push('\n');
        }
        s
    }
    fn pos(&self, id: u64) -> Option<usize> {
        self.lines.iter().position(|l| l.id == id)
    }
    fn text_of(&self, id: u64) -> &str {
        &self.lines[self.pos(id).expect("id")].text
    }
    fn apply(&mut self, it: &Intent) {
        let s = self.pos(it.first).expect("first");
        let e = self.pos(it.last).expect("last");
        match it.kind {
            Kind::Replace | Kind::Delete => {
                self.lines.splice(s..=e, it.body.clone());
            }
            Kind::InsertAfter => {
                self.lines.splice(s + 1..s + 1, it.body.clone());
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Replace,
    Delete,
    InsertAfter,
}

#[derive(Clone)]
struct Intent {
    kind: Kind,
    first: u64,
    last: u64,
    body: Vec<Line>,
}

struct Step {
    external: Option<(u64, Vec<Line>)>,
    intents: Vec<Intent>,
}

struct Task {
    tag: String,
    original: Doc,
    steps: Vec<Step>,
}

fn gen_task(doc: &Doc, rng: &mut Rng, tag: &str, p_ext: f64) -> Task {
    let n = doc.lines.len();
    let k = rng.range(1, 5);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut planned: Vec<(usize, Intent)> = Vec::new();
    let mut next_id = 10_000_000u64;
    for c in 0..k {
        for _ in 0..50 {
            let r = rng.below(100);
            let (kind, len) = if r < 45 {
                (Kind::Replace, 1)
            } else if r < 75 {
                (Kind::Replace, rng.range(2, 8))
            } else if r < 85 {
                (Kind::Delete, rng.range(1, 4))
            } else {
                (Kind::InsertAfter, 1)
            };
            if n < len + 2 {
                continue;
            }
            let s = rng.below(n - len + 1);
            let e = s + len - 1;
            if spans.iter().any(|&(a, b)| s <= b + 1 && a <= e + 1) {
                continue;
            }
            spans.push((s, e));
            let indent: String = doc.lines[s]
                .text
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let count = match kind {
                Kind::Replace => rng.range(1, len + 2),
                Kind::Delete => 0,
                Kind::InsertAfter => rng.range(1, 3),
            };
            let mut body = Vec::new();
            for i in 0..count {
                next_id += 1;
                body.push(Line {
                    id: next_id,
                    text: format!("{indent}let bench_{tag}_{c}_{i} = {};", rng.below(1000)),
                });
            }
            planned.push((
                s,
                Intent {
                    kind,
                    first: doc.lines[s].id,
                    last: doc.lines[e].id,
                    body,
                },
            ));
            break;
        }
    }
    if rng.chance(0.5) {
        planned.sort_by_key(|(s, _)| *s);
    } else {
        for i in (1..planned.len()).rev() {
            let j = rng.below(i + 1);
            planned.swap(i, j);
        }
    }
    let intents: Vec<Intent> = planned.into_iter().map(|(_, it)| it).collect();
    let groups: Vec<Vec<Intent>> = if intents.len() > 1 && rng.chance(0.4) {
        vec![intents]
    } else {
        intents.into_iter().map(|it| vec![it]).collect()
    };
    let free: Vec<usize> = (0..n)
        .filter(|i| !spans.iter().any(|&(s, e)| (s..=e).contains(i)))
        .collect();
    let mut steps = Vec::new();
    for (g, intents) in groups.into_iter().enumerate() {
        let external = if !free.is_empty() && rng.chance(p_ext) {
            let at = doc.lines[free[rng.below(free.len())]].id;
            let mut lines = Vec::new();
            for j in 0..rng.range(1, 3) {
                next_id += 1;
                lines.push(Line {
                    id: next_id,
                    text: format!("// external edit {tag} {g} {j}"),
                });
            }
            Some((at, lines))
        } else {
            None
        };
        steps.push(Step { external, intents });
    }
    Task {
        tag: tag.to_string(),
        original: doc.clone(),
        steps,
    }
}

fn mutate(c: char, rng: &mut Rng) -> char {
    const POOL: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789 _(){};";
    loop {
        let m = POOL[rng.below(POOL.len())] as char;
        if m != c {
            return m;
        }
    }
}

fn copy(s: &str, rng: &mut Rng, eps: f64) -> String {
    if eps == 0.0 {
        return s.to_string();
    }
    s.chars()
        .map(|c| if rng.chance(eps) { mutate(c, rng) } else { c })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum ToolKind {
    Hashline,
}

impl ToolKind {
    fn name(self) -> &'static str {
        match self {
            ToolKind::Hashline => "edit_file",
        }
    }
}

#[derive(Default)]
struct Stats {
    tasks: usize,
    ok: usize,
    silent: usize,
    calls: usize,
    first_fail: usize,
    attempts: usize,
    attempt_fail: usize,
    reads: usize,
    read_bytes: usize,
    arg_bytes: usize,
    out_bytes: usize,
    time: Duration,
    errors: HashMap<String, usize>,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.tasks += other.tasks;
        self.ok += other.ok;
        self.silent += other.silent;
        self.calls += other.calls;
        self.first_fail += other.first_fail;
        self.attempts += other.attempts;
        self.attempt_fail += other.attempt_fail;
        self.reads += other.reads;
        self.read_bytes += other.read_bytes;
        self.arg_bytes += other.arg_bytes;
        self.out_bytes += other.out_bytes;
        self.time += other.time;
        for (k, v) in &other.errors {
            *self.errors.entry(k.clone()).or_default() += v;
        }
    }
}

struct Know {
    view: Doc,
    seen: HashSet<u64>,
    anchors: HashMap<u64, usize>,
}

fn windows(doc: &Doc, ids: &[u64], radius: Option<usize>) -> Vec<(usize, usize)> {
    let n = doc.lines.len();
    let Some(r) = radius else {
        return vec![(1, n)];
    };
    let mut spans: Vec<(usize, usize)> = ids
        .iter()
        .filter_map(|&id| doc.pos(id))
        .map(|p| (p.saturating_sub(r) + 1, (p + r + 1).min(n)))
        .collect();
    spans.sort();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        if let Some(last) = out.last_mut()
            && s <= last.1 + 1
        {
            last.1 = last.1.max(e);
            continue;
        }
        out.push((s, e));
    }
    out
}

async fn read_windows(
    ctx: &ToolExecutionContext,
    path: &Path,
    wins: &[(usize, usize)],
    st: &mut Stats,
) {
    for &(s, e) in wins {
        let mut start = s;
        while start <= e {
            let max = (e - start + 1).min(1000);
            let args = json!({ "path": path, "start_line": start, "max_lines": max });
            let t = Instant::now();
            let out = ReadFileTool
                .execute_output_live(args, None, ctx.clone())
                .await;
            st.time += t.elapsed();
            st.reads += 1;
            st.read_bytes += match out {
                Ok(o) => o.content.len(),
                Err(e) => e.len(),
            };
            start += max;
        }
    }
}

fn learn(k: &mut Know, oracle: &Doc, wins: &[(usize, usize)]) {
    for &(s, e) in wins {
        for idx in s - 1..e {
            let id = oracle.lines[idx].id;
            k.seen.insert(id);
            k.anchors.insert(id, idx + 1);
        }
    }
    k.view = oracle.clone();
}

fn step_ids(intents: &[Intent]) -> Vec<u64> {
    intents.iter().flat_map(|it| [it.first, it.last]).collect()
}

fn edit_file_hunk(k: &Know, it: &Intent, rng: &mut Rng, eps: f64) -> Value {
    let anchor = |id: u64, rng: &mut Rng| {
        let line = k.anchors[&id];
        copy(
            &format!("{line}#{}", line_hash(k.view.text_of(id))),
            rng,
            eps,
        )
    };
    let text: Vec<&str> = it.body.iter().map(|l| l.text.as_str()).collect();
    let mut obj = Map::new();
    match it.kind {
        Kind::InsertAfter => {
            obj.insert("after".into(), anchor(it.first, rng).into());
        }
        _ => {
            obj.insert("at".into(), anchor(it.first, rng).into());
            if it.last != it.first {
                obj.insert("end".into(), anchor(it.last, rng).into());
            }
        }
    }
    obj.insert("text".into(), text.join("\n").into());
    Value::Object(obj)
}

fn build_args(path: &Path, k: &Know, intents: &[Intent], rng: &mut Rng, eps: f64) -> Value {
    let hunks: Vec<Value> = intents
        .iter()
        .map(|it| edit_file_hunk(k, it, rng, eps))
        .collect();
    if hunks.len() == 1 {
        let mut obj = Map::new();
        obj.insert("path".into(), json!(path));
        if let Value::Object(h) = hunks.into_iter().next().unwrap() {
            obj.extend(h);
        }
        Value::Object(obj)
    } else {
        json!({ "path": path, "edits": hunks })
    }
}

fn error_key(err: &str) -> String {
    let line = err
        .lines()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("no changes written")
        })
        .unwrap_or(err);
    let mut key = String::new();
    let mut in_tick = false;
    for c in line.chars() {
        if c == '`' {
            if !in_tick {
                key.push_str("<x>");
            }
            in_tick = !in_tick;
        } else if !in_tick {
            key.push(if c.is_ascii_digit() { '9' } else { c });
        }
    }
    let key = key.trim();
    if key.is_empty() {
        return "<empty>".into();
    }
    key.chars().take(70).collect()
}

async fn run_task(task: &Task, tool: ToolKind, window: bool, eps: f64, seed: u64) -> Stats {
    let mut st = Stats {
        tasks: 1,
        ..Stats::default()
    };
    let mut rng = Rng(seed);
    let path: PathBuf = common::temp_path(&format!("bench-{}-{}.rs", task.tag, tool.name()));
    let mut oracle = task.original.clone();
    tokio::fs::write(&path, oracle.text()).await.unwrap();
    let ctx = ToolExecutionContext::default();
    if tool == ToolKind::Hashline {
        ctx.snapshots.write().unwrap().set_hashline(true);
    }
    let radius = |level: u32| window.then(|| 15 * 3usize.pow(level));
    let all_ids: Vec<u64> = task
        .steps
        .iter()
        .flat_map(|s| step_ids(&s.intents))
        .collect();
    let mut k = Know {
        view: oracle.clone(),
        seen: HashSet::new(),
        anchors: HashMap::new(),
    };
    let wins = windows(&oracle, &all_ids, radius(0));
    read_windows(&ctx, &path, &wins, &mut st).await;
    learn(&mut k, &oracle, &wins);

    'steps: for step in &task.steps {
        if let Some((at, lines)) = &step.external {
            let p = oracle.pos(*at).unwrap_or(oracle.lines.len());
            oracle.lines.splice(p..p, lines.clone());
            tokio::fs::write(&path, oracle.text()).await.unwrap();
        }
        let mut expected = oracle.clone();
        for it in &step.intents {
            expected.apply(it);
        }
        let expected_text = expected.text();
        let ids = step_ids(&step.intents);
        st.calls += 1;
        for attempt in 0..3u32 {
            let args = build_args(&path, &k, &step.intents, &mut rng, eps);
            let args_dbg = args.clone();
            st.attempts += 1;
            st.arg_bytes += serde_json::to_string(&args).unwrap().len();
            let t = Instant::now();
            let result = EditFileTool
                .execute_output_live(args, None, ctx.clone())
                .await;
            st.time += t.elapsed();
            match result {
                Ok(out) => {
                    st.out_bytes += out.content.len();
                    let actual = tokio::fs::read_to_string(&path).await.unwrap();
                    if actual != expected_text {
                        if std::env::var_os("BENCH_DEBUG").is_some() {
                            let a: Vec<&str> = actual.lines().collect();
                            let e: Vec<&str> = expected_text.lines().collect();
                            let i = (0..a.len().max(e.len()))
                                .find(|&i| a.get(i) != e.get(i))
                                .unwrap_or(0);
                            eprintln!(
                                "SILENT {} {} attempt {attempt} line {}\n  args: {}\n  expected: {:?}\n  actual:   {:?}\n  intents: {:?}",
                                tool.name(),
                                task.tag,
                                i + 1,
                                args_dbg,
                                &e[i.saturating_sub(2)..(i + 3).min(e.len())],
                                &a[i.saturating_sub(2)..(i + 3).min(a.len())],
                                step.intents
                                    .iter()
                                    .map(|it| (it.kind, it.first, it.last))
                                    .collect::<Vec<_>>(),
                            );
                        }
                        st.silent += 1;
                        break 'steps;
                    }
                    oracle = expected;
                    for it in &step.intents {
                        k.view.apply(it);
                        k.seen.extend(it.body.iter().map(|l| l.id));
                    }
                    continue 'steps;
                }
                Err(err) => {
                    st.out_bytes += err.len();
                    st.attempt_fail += 1;
                    if attempt == 0 {
                        st.first_fail += 1;
                    }
                    *st.errors.entry(error_key(&err)).or_default() += 1;
                    if attempt == 2 {
                        break 'steps;
                    }
                    if tool == ToolKind::Hashline {
                        let fresh: Vec<(u64, usize)> = ids
                            .iter()
                            .map(|&id| (id, oracle.pos(id).unwrap() + 1))
                            .collect();
                        if fresh
                            .iter()
                            .all(|&(id, n)| err.contains(&render_line(n, oracle.text_of(id))))
                        {
                            for (id, n) in fresh {
                                k.anchors.insert(id, n);
                            }
                            k.view = oracle.clone();
                            continue;
                        }
                    }
                    let wins = windows(&oracle, &ids, radius(attempt + 1));
                    read_windows(&ctx, &path, &wins, &mut st).await;
                    learn(&mut k, &oracle, &wins);
                }
            }
        }
    }
    let final_text = tokio::fs::read_to_string(&path).await.unwrap();
    if st.silent == 0
        && final_text == oracle.text()
        && oracle.text() == {
            let mut done = task.original.clone();
            for step in &task.steps {
                if let Some((at, lines)) = &step.external {
                    let p = done.pos(*at).unwrap_or(done.lines.len());
                    done.lines.splice(p..p, lines.clone());
                }
                for it in &step.intents {
                    done.apply(it);
                }
            }
            done.text()
        }
    {
        st.ok = 1;
    }
    let _ = tokio::fs::remove_file(&path).await;
    st
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn corpus() -> Vec<(String, Doc)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let docs: Vec<(String, Doc)> = files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let n = text.lines().count();
            let ok = (150..=1500).contains(&n)
                && !text.contains('\r')
                && text.lines().all(|l| l.len() <= 300);
            ok.then(|| {
                let name = p.strip_prefix(&root).unwrap().display().to_string();
                (name, Doc::new(&text))
            })
        })
        .collect();
    let limit: usize = std::env::var("BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let step = (docs.len() / limit.max(1)).max(1);
    docs.into_iter().step_by(step).take(limit).collect()
}

struct Cfg {
    name: &'static str,
    window: bool,
    eps: f64,
    p_ext: f64,
}

fn row(label: &str, tool: ToolKind, s: &Stats) {
    let t = s.tasks.max(1) as f64;
    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    let tok = |b: usize| b as f64 / 4.0 / t;
    println!(
        "{:<24} {:<13} {:>6.1} {:>7.1} {:>7.1} {:>6} {:>6.2} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>7.2}",
        label,
        tool.name(),
        pct(s.ok, s.tasks),
        pct(s.first_fail, s.calls),
        pct(s.attempt_fail, s.attempts),
        s.silent,
        s.reads as f64 / t,
        tok(s.read_bytes),
        tok(s.arg_bytes),
        tok(s.out_bytes),
        tok(s.read_bytes + s.arg_bytes + s.out_bytes),
        s.time.as_secs_f64() * 1000.0 / t,
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn edit_file_bench() {
    let seeds: u64 = std::env::var("BENCH_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let docs = corpus();
    let configs = [
        Cfg {
            name: "full clean",
            window: false,
            eps: 0.0,
            p_ext: 0.0,
        },
        Cfg {
            name: "full external",
            window: false,
            eps: 0.0,
            p_ext: 0.3,
        },
        Cfg {
            name: "window clean",
            window: true,
            eps: 0.0,
            p_ext: 0.0,
        },
        Cfg {
            name: "window external",
            window: true,
            eps: 0.0,
            p_ext: 0.3,
        },
        Cfg {
            name: "full noisy",
            window: false,
            eps: 0.001,
            p_ext: 0.0,
        },
        Cfg {
            name: "window noisy+external",
            window: true,
            eps: 0.001,
            p_ext: 0.3,
        },
    ];
    println!(
        "\ncorpus: {} files, {} seeds/file/config; tokens ~= bytes/4, per task\n",
        docs.len(),
        seeds
    );
    println!(
        "{:<24} {:<13} {:>6} {:>7} {:>7} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>7}",
        "config",
        "tool",
        "ok%",
        "fail1%",
        "failA%",
        "silent",
        "reads",
        "readTok",
        "argTok",
        "outTok",
        "totTok",
        "ms"
    );
    let tools = [ToolKind::Hashline];
    let mut totals = [Stats::default(), Stats::default()];
    for (ci, cfg) in configs.iter().enumerate() {
        let mut per = [Stats::default(), Stats::default()];
        for (fi, (_name, doc)) in docs.iter().enumerate() {
            for s in 0..seeds {
                let seed = (ci as u64) << 40 | (fi as u64) << 20 | s;
                let mut rng = Rng(seed ^ 0xA5A5_5A5A);
                let task = gen_task(doc, &mut rng, &format!("{ci}_{fi}_{s}"), cfg.p_ext);
                for (ti, tool) in tools.iter().enumerate() {
                    let st = run_task(&task, *tool, cfg.window, cfg.eps, seed).await;
                    per[ti].add(&st);
                }
            }
        }
        for (ti, tool) in tools.iter().enumerate() {
            row(cfg.name, *tool, &per[ti]);
            totals[ti].add(&per[ti]);
        }
    }
    println!();
    for (ti, tool) in tools.iter().enumerate() {
        row("ALL", *tool, &totals[ti]);
    }
    for (ti, tool) in tools.iter().enumerate() {
        let mut errs: Vec<(&String, &usize)> = totals[ti].errors.iter().collect();
        errs.sort_by(|a, b| b.1.cmp(a.1));
        println!("\ntop {} errors:", tool.name());
        for (k, v) in errs.into_iter().take(8) {
            println!("  {v:>5}  {k}");
        }
    }
}
