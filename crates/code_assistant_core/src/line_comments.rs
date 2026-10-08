//! Comments the user attaches to lines of a file before sending a message.
//!
//! The composer collects them as one [`DraftAttachment::LineComments`]
//! (crate::persistence::DraftAttachment). On send they become a single text
//! block wrapped in `<line-comments>`, carrying each comment's file, line
//! range, the selected lines themselves and the comment. The excerpt lets the
//! model see the code without reading the file and find the place again once
//! line numbers have shifted. [`parse`] reads such a block back, so a
//! frontend can render it compactly in the transcript.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Lines kept at most per excerpt; longer selections keep their first and
/// last lines around an elision marker.
pub const MAX_EXCERPT_LINES: usize = 40;

const OPEN_TAG: &str = "<line-comments>";
const CLOSE_TAG: &str = "</line-comments>";

/// One comment on lines `start_line..=end_line` (1-based) of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineComment {
    /// Unique within the draft, for editing and removing.
    pub id: u64,
    /// The file; absolute while in the draft, relative to the project root
    /// once sent (when it lies inside the project).
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    /// The lines count in the old version of the file (a comment on deleted
    /// lines of a diff).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub old_side: bool,
    /// Made on a diff (the Review view) rather than on the file itself;
    /// tells a frontend where to show it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_diff: bool,
    /// The selected lines as shown when the comment was made.
    pub excerpt: String,
    pub text: String,
}

impl LineComment {
    /// `"12"` or `"12-15"`.
    pub fn lines_label(&self) -> String {
        if self.start_line == self.end_line {
            self.start_line.to_string()
        } else {
            format!("{}-{}", self.start_line, self.end_line)
        }
    }

    /// The file's name, for compact labels.
    pub fn file_name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.display().to_string())
    }
}

/// `comments` with their files relative to `project_root` where they lie
/// inside it.
pub fn relative_to(comments: &[LineComment], project_root: &Path) -> Vec<LineComment> {
    comments
        .iter()
        .map(|c| {
            let mut c = c.clone();
            if let Ok(rel) = c.file.strip_prefix(project_root) {
                c.file = rel.to_path_buf();
            }
            c
        })
        .collect()
}

/// The text block sent to the model for `comments`.
pub fn render(comments: &[LineComment]) -> String {
    let mut out = String::from(OPEN_TAG);
    out.push('\n');
    for c in comments {
        let side = if c.old_side { " side=\"old\"" } else { "" };
        out.push_str(&format!(
            "<comment path=\"{}\" lines=\"{}\"{side}>\n<code>\n{}\n</code>\n{}\n</comment>\n",
            c.file.display(),
            c.lines_label(),
            cap_excerpt(&c.excerpt),
            c.text.trim(),
        ));
    }
    out.push_str(CLOSE_TAG);
    out
}

/// Keep the first and last lines of a long excerpt.
fn cap_excerpt(excerpt: &str) -> String {
    let lines: Vec<&str> = excerpt.lines().collect();
    if lines.len() <= MAX_EXCERPT_LINES {
        return excerpt.trim_end_matches('\n').to_owned();
    }
    let keep = MAX_EXCERPT_LINES / 2;
    let elided = lines.len() - 2 * keep;
    let mut out: Vec<String> = lines[..keep].iter().map(|l| l.to_string()).collect();
    out.push(format!("… {elided} lines …"));
    out.extend(lines[lines.len() - keep..].iter().map(|l| l.to_string()));
    out.join("\n")
}

/// Where `comment`'s excerpt is in `lines` now (1-based, inclusive): its
/// stored range when the lines there still match, else the occurrence
/// nearest to it, else `None` (the lines are gone). Trailing whitespace is
/// ignored.
pub fn locate(lines: &[&str], comment: &LineComment) -> Option<(usize, usize)> {
    let excerpt: Vec<&str> = comment.excerpt.lines().map(str::trim_end).collect();
    if excerpt.is_empty() || excerpt.len() > lines.len() {
        return None;
    }
    let matches_at = |start: usize| {
        lines[start..start + excerpt.len()]
            .iter()
            .zip(&excerpt)
            .all(|(line, wanted)| line.trim_end() == *wanted)
    };
    let len = excerpt.len();
    let stored = comment.start_line.saturating_sub(1);
    if stored + len <= lines.len() && matches_at(stored) {
        return Some((stored + 1, stored + len));
    }
    (0..=lines.len() - len)
        .filter(|&start| matches_at(start))
        .min_by_key(|&start| start.abs_diff(stored))
        .map(|start| (start + 1, start + len))
}

/// A comment read back from a rendered block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedComment {
    pub path: String,
    pub lines: String,
    pub code: String,
    pub text: String,
}

/// Read a block produced by [`render`]; `None` for any other text.
pub fn parse(block: &str) -> Option<Vec<ParsedComment>> {
    let body = block
        .trim()
        .strip_prefix(OPEN_TAG)?
        .strip_suffix(CLOSE_TAG)?;
    let mut comments = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<comment ") {
        let after = &rest[start + "<comment ".len()..];
        let head_end = after.find('>')?;
        let head = &after[..head_end];
        let inner_and_rest = &after[head_end + 1..];
        let end = inner_and_rest.find("</comment>")?;
        let inner = &inner_and_rest[..end];
        let code_start = inner.find("<code>\n")? + "<code>\n".len();
        let code_end = inner.find("\n</code>")?;
        comments.push(ParsedComment {
            path: attribute(head, "path")?,
            lines: attribute(head, "lines")?,
            code: inner[code_start..code_end].to_owned(),
            text: inner[code_end + "\n</code>".len()..].trim().to_owned(),
        });
        rest = &inner_and_rest[end + "</comment>".len()..];
    }
    Some(comments)
}

fn attribute(head: &str, name: &str) -> Option<String> {
    let start = head.find(&format!("{name}=\""))? + name.len() + 2;
    let len = head[start..].find('"')?;
    Some(head[start..start + len].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(file: &str, lines: (usize, usize), excerpt: &str, text: &str) -> LineComment {
        LineComment {
            id: 1,
            file: PathBuf::from(file),
            start_line: lines.0,
            end_line: lines.1,
            old_side: false,
            in_diff: false,
            excerpt: excerpt.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn renders_paths_lines_code_and_text_and_parses_back() {
        let comments = vec![
            comment(
                "/p/src/lib.rs",
                (41, 42),
                "let x = y.unwrap();\nx",
                "no unwrap",
            ),
            comment("/elsewhere/a.md", (3, 3), "# Title", "rename"),
        ];
        let block = render(&relative_to(&comments, Path::new("/p")));
        assert_eq!(
            block,
            "<line-comments>\n\
             <comment path=\"src/lib.rs\" lines=\"41-42\">\n<code>\nlet x = y.unwrap();\nx\n</code>\nno unwrap\n</comment>\n\
             <comment path=\"/elsewhere/a.md\" lines=\"3\">\n<code>\n# Title\n</code>\nrename\n</comment>\n\
             </line-comments>"
        );
        let parsed = parse(&block).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "src/lib.rs");
        assert_eq!(parsed[0].lines, "41-42");
        assert_eq!(parsed[0].code, "let x = y.unwrap();\nx");
        assert_eq!(parsed[0].text, "no unwrap");
        assert!(parse("just text").is_none());
    }

    #[test]
    fn long_excerpts_keep_their_ends() {
        let excerpt: Vec<String> = (1..=100).map(|i| format!("l{i}")).collect();
        let capped = cap_excerpt(&excerpt.join("\n"));
        let lines: Vec<&str> = capped.lines().collect();
        assert_eq!(lines.len(), MAX_EXCERPT_LINES + 1);
        assert_eq!(lines[0], "l1");
        assert_eq!(lines[20], "… 60 lines …");
        assert_eq!(lines.last(), Some(&"l100"));
    }

    #[test]
    fn locate_follows_moved_lines() {
        let c = comment("a.rs", (2, 3), "b\nc", "x");
        assert_eq!(locate(&["a", "b", "c"], &c), Some((2, 3)));
        assert_eq!(locate(&["new", "a", "b  ", "c"], &c), Some((3, 4)));
        // Two occurrences: the nearer one wins.
        assert_eq!(
            locate(
                &["b", "c", "x", "x", "x", "x", "b", "c"],
                &comment("a.rs", (6, 7), "b\nc", "")
            ),
            Some((7, 8))
        );
        assert_eq!(locate(&["a", "z"], &c), None);
    }

    #[test]
    fn old_side_comments_say_so() {
        let mut c = comment("a.rs", (5, 5), "gone", "why removed?");
        c.old_side = true;
        assert!(render(&[c]).contains("lines=\"5\" side=\"old\""));
    }
}
