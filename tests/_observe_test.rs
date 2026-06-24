use bone::ui::render::markdown;
use bone::ui::render::safe_markdown_prefix_end;

fn to_texts(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines.iter().map(|l| l.spans.iter().map(|s| s.content.clone()).collect()).collect()
}

fn dedup_fold(acc: &mut Vec<String>, last_blank: &mut bool, lines: &[String]) {
    for line in lines {
        let blank = line.trim().is_empty();
        if blank && *last_blank { continue; }
        acc.push(line.clone());
        *last_blank = blank;
    }
}

fn sim_old(content: &str, width: u16) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut last_blank = false;
    let mut flushed = 0usize;
    let mut prev_safe = 0usize;
    for i in 1..=content.len() {
        let safe = safe_markdown_prefix_end(&content[..i]);
        if safe > prev_safe {
            let rendered = to_texts(&markdown::render_markdown(&content[..safe], width));
            if flushed < rendered.len() {
                dedup_fold(&mut out, &mut last_blank, &rendered[flushed..]);
                flushed = rendered.len();
            }
            prev_safe = safe;
        }
    }
    let rendered = to_texts(&markdown::render_markdown(content, width));
    if flushed < rendered.len() {
        dedup_fold(&mut out, &mut last_blank, &rendered[flushed..]);
    }
    out
}

fn sim_new(content: &str, width: u16) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut last_blank = false;
    let mut flushed = 0usize;
    let mut prev_safe = 0usize;
    let mut has_prior = false;
    for i in 1..=content.len() {
        let safe = safe_markdown_prefix_end(&content[..i]);
        if safe > prev_safe {
            let delta = &content[flushed..safe];
            let mut rendered = to_texts(&markdown::render_markdown(delta, width));
            if !rendered.is_empty() && has_prior
                && !markdown::same_container_boundary(&content[..safe], delta)
            {
                rendered.insert(0, String::new());
            }
            if !rendered.is_empty() {
                has_prior = true;
                dedup_fold(&mut out, &mut last_blank, &rendered);
            }
            flushed = safe;
            prev_safe = safe;
        }
    }
    if flushed < content.len() {
        let delta = &content[flushed..];
        let mut rendered = to_texts(&markdown::render_markdown(delta, width));
        if !rendered.is_empty() && has_prior
            && !markdown::same_container_boundary(&content[..flushed], delta)
        {
            rendered.insert(0, String::new());
        }
        if !rendered.is_empty() {
            dedup_fold(&mut out, &mut last_blank, &rendered);
        }
    }
    out
}

const CASES: &[&str] = &[
    "", "Hello\n", "Hello\n\n", "Hello\n\nWorld\n", "Hello\n\nWorld\n\nDone\n",
    "para1\n\n\npara2\n", "para1\n\n\n\npara2\n",
    "# Heading\n\nbody\n", "1. first\n\n2. second\n", "1. a\n2. b\n3. c\n",
    "intro\n\n```rust\nfn a() {}\n```\n\ntail\n", "intro\n\n```rust\nfn a() {}\n```\n",
    "text ```inline``` more\n\nafter\n",
    "| a | b |\n|---|---|\n| 1 | 2 |\n", "before\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter\n",
    "> quote line\n> another\n\nafter\n", "line one\nline two still same para\n\nsecond\n",
    "- a\n- b\n- c\n", "para\n\n\n\n\npara2\n", "a\n\nb\n\nc\n\nd\n",
    "-\n\n-", "- x\n\n- y\n", "> inner\n\noutside\n", "```\ncode\n```\n\npara\n",
    "# H1\n\n## H2\n\ntext\n",
];

#[test]
fn delta_render_matches_full_render() {
    for width in [40u16, 80u16] {
        let mut failures = 0;
        for (idx, case) in CASES.iter().enumerate() {
            let old = sim_old(case, width);
            let new = sim_new(case, width);
            if old != new {
                failures += 1;
                if failures <= 10 {
                    println!("MISMATCH width={width} case[{idx}]={case:?}");
                    println!("  old: {old:?}");
                    println!("  new: {new:?}");
                }
            }
        }
        assert_eq!(failures, 0, "{failures} case(s) diverged at width={width}");
    }
}

#[test]
fn fuzz_delta_render_matches_full_render() {
    let alphabet = "abc \n\n```x\n#>|-123\n";
    let mut rng: u64 = 12345;
    for width in [40u16, 80u16] {
        let mut fail = 0;
        for _ in 0..2000 {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            let len = (rng % 16) as usize;
            let bytes = alphabet.as_bytes();
            let mut s = String::new();
            for _ in 0..len {
                rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                s.push(bytes[(rng as usize) % bytes.len()] as char);
            }
            if sim_old(&s, width) != sim_new(&s, width) {
                fail += 1;
                if fail <= 5 {
                    println!("FUZZ MISMATCH width={width} {s:?}\n  old {:?}\n  new {:?}",
                        sim_old(&s, width), sim_new(&s, width));
                }
            }
        }
        assert_eq!(fail, 0, "{fail} fuzz cases diverged at width={width}");
    }
}
