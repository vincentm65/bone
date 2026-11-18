//! Markdown as data. `parse` splits text into blocks (headings, paragraphs,
//! list items, quotes, code, rules, blank lines) with inline spans marked
//! bold/italic/code/link. Lua decides how they look (`bone.markdown.parse`).
//! `<#f80>text</>` colors its text (a span's `color`, as `#rrggbb`) in prose
//! and in code blocks without a language (or `text`). Runs nest; one left
//! open runs to the end of its block.

use serde_json::{Value, json};

/// Parse text into blocks. Runs of blank lines collapse to one; blank lines
/// at the start and end are dropped. An unclosed fence (mid-stream) runs to
/// the end.
pub fn parse(text: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    // An open code block: its fence (only the same one closes it), language, lines.
    let mut code: Option<(&str, Option<String>, Vec<String>)> = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        let raw = lines[i];
        i += 1;
        let trimmed = raw.trim_start();
        let fence = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f));
        if let Some(f) = fence.filter(|f| code.as_ref().is_none_or(|(m, ..)| m == f)) {
            match code.take() {
                Some((_, lang, lines)) => out.push(code_block(lang, &lines, false)),
                None => {
                    let lang = trimmed[3..].trim();
                    code = Some((f, (!lang.is_empty()).then(|| lang.to_owned()), Vec::new()));
                }
            }
            continue;
        }
        if let Some((_, _, lines)) = &mut code {
            lines.push(raw.to_owned());
            continue;
        }
        if trimmed.is_empty() {
            if out.last().is_some_and(|b| b["kind"] != "blank") {
                out.push(json!({ "kind": "blank" }));
            }
            continue;
        }
        let indent = raw.len() - trimmed.len();

        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push(json!({ "kind": "heading", "level": hashes, "spans": inline(trimmed[hashes + 1..].trim()) }));
            continue;
        }
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() >= 3
            && ['-', '*', '_']
                .iter()
                .any(|&r| compact.chars().all(|c| c == r))
        {
            out.push(json!({ "kind": "rule" }));
            continue;
        }
        if trimmed.contains('|') {
            let mut j = start;
            while j < lines.len() && lines[j].contains('|') {
                j += 1;
            }
            if j - start >= 2 {
                let sep = lines[start + 1];
                let sep_row = is_header_separator(sep);
                let rows: Vec<Value> = (start..j)
                    .filter(|&k| !is_separator_row(lines[k]))
                    .map(|k| {
                        Value::Array(
                            split_row(lines[k])
                                .into_iter()
                                .map(|c| Value::Array(inline(c.trim())))
                                .collect(),
                        )
                    })
                    .collect();
                if !rows.is_empty() {
                    out.push(json!({
                        "kind": "table",
                        "header": sep_row,
                        "align": if sep_row { Some(table_align(sep)) } else { None },
                        "rows": rows,
                    }));
                    i = j;
                    continue;
                }
            } else {
                out.push(
                    json!({ "kind": "paragraph", "indent": indent, "spans": inline(trimmed) }),
                );
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            out.push(json!({ "kind": "quote", "spans": inline(rest.trim_start()) }));
            continue;
        }
        if let Some((marker, body)) = list_item(trimmed) {
            out.push(json!({
                "kind": "item",
                "indent": indent,
                "marker": marker,
                "ordered": marker.ends_with(['.', ')']),
                "spans": inline(body),
            }));
            continue;
        }
        out.push(json!({ "kind": "paragraph", "indent": indent, "spans": inline(trimmed) }));
    }
    if let Some((_, lang, lines)) = code {
        out.push(code_block(lang, &lines, true));
    }
    while out.last().is_some_and(|b| b["kind"] == "blank") {
        out.pop();
    }
    out
}

/// A code block: `lines` its text as shown, `spans` each line's
/// `{ text, color? }` spans. Only a block without a language (or `text`)
/// takes color tags, and a run may cross its lines.
fn code_block(lang: Option<String>, lines: &[String], open: bool) -> Value {
    let (tags, mut colors) = (lang.as_deref().is_none_or(|l| l == "text"), Vec::new());
    let spans: Vec<Vec<Value>> = lines.iter().map(|l| literal(l, tags, &mut colors)).collect();
    let shown: Vec<String> = (spans.iter())
        .map(|l| l.iter().filter_map(|s| s["text"].as_str()).collect())
        .collect();
    let mut b = json!({ "kind": "code", "lang": lang, "lines": shown, "spans": spans });
    if open {
        b["open"] = json!(true);
    }
    b
}

/// A color tag at `c[i]`: `<#rgb>` / `<#rrggbb>` opens a run (`Some` color,
/// as `#rrggbb`), `</>` closes the innermost (`None`; text unless `open`).
/// With where the text goes on.
fn tag(c: &[char], i: usize, open: bool) -> Option<(Option<String>, usize)> {
    if c[i] != '<' {
        return None;
    }
    if c[i..].starts_with(&['<', '/', '>']) {
        return open.then_some((None, i + 3));
    }
    let hex: String = c[i + 1..].iter().take(8).take_while(|&&ch| ch != '>').collect();
    let at = i + 2 + hex.chars().count();
    if c.get(at - 1) != Some(&'>') || !matches!(hex.len(), 4 | 7) {
        return None;
    }
    let (r, g, b, _) = crate::theme::parse_hex(&hex)?;
    Some((Some(format!("#{r:02x}{g:02x}{b:02x}")), at))
}

/// Text taken as written, but for color tags (when `tags`), as
/// `{ text, color? }` spans. `colors`: the runs open before and after.
fn literal(text: &str, tags: bool, colors: &mut Vec<String>) -> Vec<Value> {
    let c: Vec<char> = text.chars().collect();
    let (mut out, mut buf, mut i) = (Vec::new(), String::new(), 0);
    loop {
        let t = (tags && i < c.len()).then(|| tag(&c, i, !colors.is_empty())).flatten();
        if i < c.len() && t.is_none() {
            buf.push(c[i]);
            i += 1;
            continue;
        }
        if !buf.is_empty() {
            let mut s = json!({ "text": std::mem::take(&mut buf) });
            paint(std::slice::from_mut(&mut s), colors.last());
            out.push(s);
        }
        let Some((col, at)) = t else { return out };
        if let Some(col) = col { colors.push(col) } else { colors.pop(); }
        i = at;
    }
}

/// Give `spans` the color of the innermost open run, if any.
fn paint(spans: &mut [Value], color: Option<&String>) {
    for s in spans.iter_mut().filter(|_| color.is_some()) {
        s["color"] = json!(color);
    }
}

/// A row like `| a | b |` to its cells (`a`, `b`); the trailing `|` is
/// optional, and `\|` is a pipe inside a cell.
fn split_row(row: &str) -> Vec<String> {
    let t = row.trim().replace("\\|", "\u{1}");
    let t = t.strip_prefix('|').unwrap_or(&t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().replace('\u{1}', "|")).collect()
}

/// `|---|:===:|`-style separator row.
fn is_separator_row(s: &str) -> bool {
    let t = s.trim();
    t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) && t.contains('-')
}

fn is_header_separator(s: &str) -> bool {
    is_separator_row(s)
        && split_row(s).into_iter().all(|cell| {
            let cell = cell.trim();
            let aligned = cell.starts_with(':') || cell.ends_with(':');
            let cell = cell.trim_matches(':');
            cell.len() >= if aligned { 2 } else { 3 } && cell.chars().all(|c| c == '-')
        })
}

/// Column alignments from a separator row: `"l"`, `"c"` or `"r"`.
fn table_align(s: &str) -> Vec<String> {
    split_row(s)
        .into_iter()
        .map(|c| {
            let l = c.starts_with(':');
            let r = c.ends_with(':');
            if l && r {
                "c".to_owned()
            } else if r {
                "r".to_owned()
            } else {
                "l".to_owned()
            }
        })
        .collect()
}

fn list_item(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix(['-', '*', '+']) {
        return rest.strip_prefix(' ').map(|body| (&s[..1], body));
    }
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let after = &s[digits..];
        if let Some(body) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some((&s[..digits + 1], body));
        }
    }
    None
}

/// Inline markup: `code`, **bold**, *italic* / _italic_, [text](url),
/// <#f80>color</>. Each span is `{ text, bold?, italic?, code?, link?, color? }`.
pub fn inline(text: &str) -> Vec<Value> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic) = (false, false);
    let flush = |buf: &mut String, out: &mut Vec<Value>, bold: bool, italic: bool| {
        if !buf.is_empty() {
            let mut s = json!({ "text": std::mem::take(buf) });
            if bold {
                s["bold"] = json!(true);
            }
            if italic {
                s["italic"] = json!(true);
            }
            out.push(s);
        }
    };
    let find = |from: usize, pat: &[char]| {
        (from..n.saturating_sub(pat.len() - 1)).find(|&j| c[j..j + pat.len()] == *pat)
    };
    let alnum = |i: usize| i < n && c[i].is_alphanumeric();
    let (mut i, mut colors, mut mark) = (0, Vec::new(), 0);
    while i < n {
        let ch = c[i];
        if ch == '`'
            && let Some(j) = find(i + 1, &['`'])
        {
            flush(&mut buf, &mut out, bold, italic);
            out.push(json!({ "text": c[i + 1..j].iter().collect::<String>(), "code": true }));
            i = j + 1;
            continue;
        }
        // A tag colors what came since the last one, then opens or closes.
        if let Some((col, at)) = tag(&c, i, !colors.is_empty()) {
            flush(&mut buf, &mut out, bold, italic);
            paint(&mut out[mark..], colors.last());
            if let Some(col) = col { colors.push(col) } else { colors.pop(); }
            (mark, i) = (out.len(), at);
            continue;
        }
        if (ch == '*' || ch == '_') && i + 1 < n && c[i + 1] == ch {
            let word_ok = ch == '*' || (i == 0 || !alnum(i - 1));
            if bold || (word_ok && find(i + 2, &[ch, ch]).is_some()) {
                flush(&mut buf, &mut out, bold, italic);
                bold = !bold;
                i += 2;
                continue;
            }
        }
        if ch == '*' || ch == '_' {
            let closes =
                italic && i > 0 && !c[i - 1].is_whitespace() && (ch == '*' || !alnum(i + 1));
            if closes {
                flush(&mut buf, &mut out, bold, italic);
                italic = false;
                i += 1;
                continue;
            }
            let opens = !italic
                && i + 1 < n
                && !c[i + 1].is_whitespace()
                && (ch == '*' || i == 0 || !alnum(i - 1))
                && (i + 2..n).any(|j| {
                    c[j] == ch && !c[j - 1].is_whitespace() && (ch == '*' || !alnum(j + 1))
                });
            if opens {
                flush(&mut buf, &mut out, bold, italic);
                italic = true;
                i += 1;
                continue;
            }
        }
        if ch == '['
            && let Some(close) = find(i + 1, &[']', '('])
            && let Some(end) = find(close + 2, &[')'])
        {
            flush(&mut buf, &mut out, bold, italic);
            // A label's tags are dropped; the link takes the color around it.
            let label = c[i + 1..close].iter().collect::<String>();
            let label = literal(&label, true, &mut colors.clone());
            let label: String = label.iter().filter_map(|s| s["text"].as_str()).collect();
            let url: String = c[close + 2..end].iter().collect();
            out.push(json!({ "text": label, "link": url }));
            i = end + 1;
            continue;
        }
        buf.push(ch);
        i += 1;
    }
    flush(&mut buf, &mut out, bold, italic);
    paint(&mut out[mark..], colors.last());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(md: &str) -> Vec<String> {
        parse(md)
            .iter()
            .map(|b| b["kind"].as_str().unwrap().to_owned())
            .collect()
    }

    fn plain(s: &str) -> String {
        inline(s)
            .iter()
            .map(|s| s["text"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn blocks() {
        let md = "\n# Title\n\n\n- one\n  - nested\n1. first\n> quoted\n\n```rust\nfn x() {}\n```\n---\ntext\n\n";
        assert_eq!(
            kinds(md),
            [
                "heading",
                "blank",
                "item",
                "item",
                "item",
                "quote",
                "blank",
                "code",
                "rule",
                "paragraph"
            ]
        );
        let b = parse(md);
        assert_eq!(b[0]["level"], 1);
        assert_eq!(
            (b[3]["indent"].as_u64(), b[3]["marker"].as_str()),
            (Some(2), Some("-"))
        );
        assert_eq!(
            (b[4]["ordered"].as_bool(), b[4]["marker"].as_str()),
            (Some(true), Some("1."))
        );
        assert_eq!(b[7]["lang"], "rust");
        assert_eq!(b[7]["lines"], json!(["fn x() {}"]));
        // An unclosed fence while streaming is still code.
        assert_eq!(parse("```\nlet a")[0]["open"], true);
    }

    #[test]
    fn inline_markup() {
        assert_eq!(
            plain("use `a*b` and **bold** and *it* or _it_"),
            "use a*b and bold and it or it"
        );
        assert_eq!(plain("snake_case_name stays"), "snake_case_name stays");
        assert_eq!(plain("2 * 3 * 4"), "2 * 3 * 4");
        let s = inline("a **b** `c` [d](http://x)");
        assert_eq!(s[1], json!({ "text": "b", "bold": true }));
        assert_eq!(s[3], json!({ "text": "c", "code": true }));
        assert_eq!(s[5], json!({ "text": "d", "link": "http://x" }));
    }
    #[test]
    fn tables() {
        let md = "| a | b |\n|---|:--:|\n| 1 | **2** |\n\na | b\n--- | :--:\n1 | **2**\n\n| x |\n\nbefore\n| p | q |\n| - | - |\n| r | s |\n";
        let b = parse(md);
        assert_eq!(
            kinds(md),
            [
                "table",
                "blank",
                "table",
                "blank",
                "paragraph",
                "blank",
                "paragraph",
                "table",
            ]
        );
        let t = &b[0];
        assert_eq!(t["header"], true);
        assert_eq!(t["align"], json!(vec!["l", "c"]));
        assert_eq!(t["rows"].as_array().unwrap().len(), 2);
        assert_eq!(t["rows"][1][1][0]["text"], "2");
        assert_eq!(t["rows"][1][1][0]["bold"], true);
        let t_no_pipe = &b[2];
        assert_eq!(t_no_pipe["header"], true);
        assert_eq!(t_no_pipe["rows"][1][1][0]["bold"], true);
        // A lone pipe line is a paragraph, not a table.
        assert_eq!(b[4]["kind"], "paragraph");
        // Without a separator row there is no header or alignment.
        let t2 = &b[7];
        assert_eq!(t2["header"], false);
        assert!(t2["align"].is_null());
        assert_eq!(t2["rows"].as_array().unwrap().len(), 2);
    }
}
