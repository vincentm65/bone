use std::collections::BTreeSet;
use std::path::Path;

use tokio::fs;

const MAX_DIRECTORY_ENTRIES: usize = 2000;
const MAX_SUGGESTIONS: usize = 3;

/// Invisible characters that map to the same visible filename, tried in both
/// directions so either spelling is repaired to the other.
const TRANSFORMS: [(char, char); 8] = [
    (' ', '\u{202f}'),
    ('\u{202f}', ' '),
    (' ', '\u{a0}'),
    ('\u{a0}', ' '),
    ('\'', '’'),
    ('\'', '‘'),
    ('’', '\''),
    ('‘', '\''),
];

/// Return a small set of invisible-spelling variants for a filename. Each
/// transform is applied once per matching character, plus one combining-mark
/// strip; single substitutions cover the realistic cases.
pub fn variants(name: &str) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (from, to) in TRANSFORMS {
        for (offset, character) in name.char_indices() {
            if character != from {
                continue;
            }
            let end = offset + character.len_utf8();
            let mut candidate =
                String::with_capacity(name.len() + to.len_utf8() - character.len_utf8());
            candidate.push_str(&name[..offset]);
            candidate.push(to);
            candidate.push_str(&name[end..]);
            found.insert(candidate);
        }
    }
    let without_combining = strip_combining(name);
    if without_combining != name {
        found.insert(without_combining);
    }
    found.remove(name);
    found.into_iter().collect()
}

fn strip_combining(input: &str) -> String {
    input
        .chars()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect()
}

/// Suggest nearby entries without turning a missing path into an implicit write.
pub async fn suggest(parent: &Path, wanted: &str) -> Vec<String> {
    let mut entries = match fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let folded_wanted = fold(wanted);
    let wanted_extension = Path::new(wanted)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    let mut scored = Vec::new();
    let mut examined = 0usize;

    while let Ok(Some(entry)) = entries.next_entry().await {
        examined += 1;
        if examined > MAX_DIRECTORY_ENTRIES {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let folded = fold(&name);
        let mut score = 0i32;
        if folded.starts_with(&folded_wanted) {
            score += 40;
        }
        if folded.contains(&folded_wanted) {
            score += 30;
        }
        if wanted_extension.is_some()
            && Path::new(&name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                == wanted_extension
        {
            score += 10;
        }
        let distance = levenshtein_at_most_two(&folded_wanted, &folded);
        if distance <= 2 {
            score += 20 - distance as i32;
        }
        if score > 0 {
            scored.push((score, name));
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, name)| name)
        .collect()
}

fn fold(value: &str) -> String {
    strip_combining(value)
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            '\u{202f}' | '\u{a0}' => ' ',
            '‘' | '’' => '\'',
            other => other,
        })
        .collect()
}

fn levenshtein_at_most_two(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 2 {
        return 3;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, right) in b.iter().enumerate() {
            current[j + 1] = (current[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + usize::from(left != right));
        }
        if current.iter().copied().min().unwrap_or(3) > 2 {
            return 3;
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_cover_invisible_filename_spellings() {
        assert!(
            variants("Screenshot 3.04 PM.png")
                .contains(&"Screenshot 3.04\u{202f}PM.png".to_string())
        );
        assert!(variants("cafe\u{301}.txt").contains(&"cafe.txt".to_string()));
    }

    #[test]
    fn levenshtein_is_bounded() {
        assert_eq!(levenshtein_at_most_two("agent.md", "agents.md"), 1);
        assert_eq!(levenshtein_at_most_two("a", "long-name"), 3);
    }
}
