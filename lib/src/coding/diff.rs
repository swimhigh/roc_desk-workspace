use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub sign: char, // '+' | '-' | ' '
    pub content: String,
}

/// Line-by-line diff for the frontend's FileChangeCard `.fcc-diff` to render
/// directly -- field shape matches `FileChangeCard.tsx`'s `DiffLine` 1:1.
pub fn generate_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|change| {
            let sign = match change.tag() {
                ChangeTag::Delete => '-',
                ChangeTag::Insert => '+',
                ChangeTag::Equal => ' ',
            };
            DiffLine {
                sign,
                content: change.to_string().trim_end_matches('\n').to_string(),
            }
        })
        .collect()
}
