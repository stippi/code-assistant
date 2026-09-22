//! What a diff card shows, computed once per content change instead of per
//! frame: [`DiffInput`] is taken from a finished tool block, its line diff
//! and syntax parse become a [`PreparedDiff`] that `BlockView` caches.
//!
//! The line diff decides the card's height, so small inputs are diffed right
//! away to keep the height stable from the first frame. The syntax parse is
//! far more expensive (compiling a grammar's queries alone takes ~10 ms) and
//! only changes colors: it always runs on a background thread, together with
//! the line diff of inputs above [`SYNC_DIFF_MAX_BYTES`].

use super::diff_card::{
    DiffLine, compute_diff_lines, get_param, parse_diff_sections, parse_match_start_lines,
    single_sided_hunk,
};
use super::diff_syntax::DiffSyntax;
use crate::blocks::ToolUseBlock;
use gpui_kit::component::highlighter::HighlightTheme;
use similar::ChangeTag;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// Inputs up to this size are diffed on the UI thread.
pub const SYNC_DIFF_MAX_BYTES: usize = 16 * 1024;

/// Old and new text of one section of a card: the whole `edit` or
/// `write_file`, or one SEARCH/REPLACE block.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SectionInput {
    pub old: String,
    pub new: String,
    /// New-file line number of the first line, when the tool reported it.
    pub start_line: Option<usize>,
}

impl SectionInput {
    /// The section's rows. A missing side gives pure additions or deletions
    /// rather than a diff against an empty line.
    pub fn diff_lines(&self) -> Vec<DiffLine> {
        let single_sided = |text: &str, tag| {
            single_sided_hunk(text, tag)
                .into_iter()
                .flat_map(|hunk| hunk.lines)
                .collect()
        };
        match (self.old.is_empty(), self.new.is_empty()) {
            (true, true) => Vec::new(),
            (true, false) => single_sided(&self.new, ChangeTag::Insert),
            (false, true) => single_sided(&self.old, ChangeTag::Delete),
            (false, false) => compute_diff_lines(&self.old, &self.new),
        }
    }
}

/// Everything a finished `edit`, `replace_in_file` or `write_file` block
/// contributes to its card's diff.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffInput {
    pub path: String,
    pub sections: Vec<SectionInput>,
    /// A `write_file` that replaced an existing file, so the card can offer
    /// both the diff and the plain new file.
    pub has_original: bool,
}

impl DiffInput {
    /// `None` for other tools and while there is nothing to show.
    pub fn for_tool(tool: &ToolUseBlock, write_file_diff_mode: bool) -> Option<Self> {
        if !matches!(
            tool.name.as_str(),
            "edit" | "replace_in_file" | "write_file"
        ) {
            return None;
        }
        let start_lines = parse_match_start_lines(tool);
        let mut has_original = false;
        let sections: Vec<SectionInput> = match tool.name.as_str() {
            "edit" => vec![SectionInput {
                old: get_param(tool, "old_text").unwrap_or_default().to_string(),
                new: get_param(tool, "new_text").unwrap_or_default().to_string(),
                start_line: start_lines.first().copied(),
            }],
            "replace_in_file" => parse_diff_sections(get_param(tool, "diff")?)
                .into_iter()
                .enumerate()
                .map(|(ix, section)| SectionInput {
                    old: section.search_content,
                    new: section.replace_content,
                    start_line: start_lines.get(ix).copied(),
                })
                .collect(),
            "write_file" => {
                let original = original_content(tool);
                has_original = original.is_some();
                vec![SectionInput {
                    old: original
                        .filter(|_| write_file_diff_mode)
                        .unwrap_or_default(),
                    new: get_param(tool, "content")?.to_string(),
                    start_line: Some(1),
                }]
            }
            _ => return None,
        };
        sections
            .iter()
            .any(|s| !s.old.is_empty() || !s.new.is_empty())
            .then(|| Self {
                path: get_param(tool, "path").unwrap_or_default().to_string(),
                sections,
                has_original,
            })
    }

    pub fn content_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    pub fn byte_len(&self) -> usize {
        self.sections
            .iter()
            .map(|s| s.old.len() + s.new.len())
            .sum()
    }

    pub fn diff(&self) -> Vec<SectionLines> {
        self.sections
            .iter()
            .map(|section| SectionLines {
                lines: section.diff_lines(),
                start_line: section.start_line,
            })
            .collect()
    }

    /// Parse every section for highlighting; `None` without a grammar for
    /// the file. Sections are snippets, so lines count from 1 on both sides.
    /// With `theme`, the line styles are computed right away too.
    /// CPU-heavy — call on a background thread.
    pub fn parse_syntax(&self, theme: Option<&HighlightTheme>) -> Option<Vec<DiffSyntax>> {
        self.sections
            .iter()
            .map(|s| {
                let syntax = DiffSyntax::parse(&self.path, Some(&s.old), Some(&s.new))?;
                if let Some(theme) = theme {
                    syntax.prime(theme);
                }
                Some(syntax)
            })
            .collect()
    }
}

/// The text a `write_file` replaced, which the tool reports in its output.
fn original_content(tool: &ToolUseBlock) -> Option<String> {
    let output: serde_json::Value = serde_json::from_str(tool.output.as_deref()?).ok()?;
    Some(output.get("original_content")?.as_str()?.to_string())
}

/// The computed rows of one section.
#[derive(Debug)]
pub struct SectionLines {
    pub lines: Vec<DiffLine>,
    pub start_line: Option<usize>,
}

/// The cached result, as handed to the card renderer. Either part can still
/// be on its way: without `sections` the card keeps showing the raw blocks it
/// streamed, without `syntax` the rows are not highlighted yet.
#[derive(Debug, Clone, Default)]
pub struct PreparedDiff {
    pub sections: Option<Rc<Vec<SectionLines>>>,
    /// One entry per section.
    pub syntax: Option<Rc<Vec<DiffSyntax>>>,
    /// See [`DiffInput::has_original`].
    pub has_original: bool,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::blocks::{ParameterBlock, ToolBlockState};
    use code_assistant_core::ui::ToolStatus;

    pub(crate) fn tool(name: &str, params: &[(&str, &str)], output: Option<&str>) -> ToolUseBlock {
        ToolUseBlock {
            name: name.to_string(),
            id: "tool-1".to_string(),
            parameters: params
                .iter()
                .map(|(name, value)| ParameterBlock {
                    name: name.to_string(),
                    value: value.to_string(),
                })
                .collect(),
            status: ToolStatus::Success,
            status_message: None,
            output: output.map(String::from),
            styled_output: None,
            state: ToolBlockState::Expanded,
            duration_seconds: None,
            images: Vec::new(),
            revision: 0,
        }
    }

    fn tags(lines: &[DiffLine]) -> Vec<ChangeTag> {
        lines.iter().map(|l| l.tag).collect()
    }

    #[test]
    fn edit_is_one_section_at_the_reported_line() {
        let tool = tool(
            "edit",
            &[
                ("path", "src/a.rs"),
                ("old_text", "let a = 1;"),
                ("new_text", "let a = 2;"),
            ],
            Some(r#"{"match_start_lines":[42]}"#),
        );
        let input = DiffInput::for_tool(&tool, true).unwrap();
        assert_eq!(input.path, "src/a.rs");
        assert_eq!(input.sections.len(), 1);
        assert_eq!(input.sections[0].start_line, Some(42));

        let prepared = input.diff();
        assert_eq!(
            tags(&prepared[0].lines),
            [ChangeTag::Delete, ChangeTag::Insert]
        );
        assert_eq!(prepared[0].start_line, Some(42));
    }

    #[test]
    fn replace_in_file_has_a_section_per_block() {
        let diff = "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n\
                    <<<<<<< SEARCH\nc\n=======\n>>>>>>> REPLACE";
        let tool = tool(
            "replace_in_file",
            &[("path", "a.txt"), ("diff", diff)],
            Some(r#"{"match_start_lines":[3,9]}"#),
        );
        let input = DiffInput::for_tool(&tool, true).unwrap();
        let starts: Vec<_> = input.sections.iter().map(|s| s.start_line).collect();
        assert_eq!(starts, [Some(3), Some(9)]);
        // An empty replacement is a pure deletion, not a diff against "".
        assert_eq!(tags(&input.sections[1].diff_lines()), [ChangeTag::Delete]);
    }

    #[test]
    fn write_file_diffs_against_the_original_only_in_diff_mode() {
        let tool = tool(
            "write_file",
            &[("path", "a.txt"), ("content", "one\ntwo\n")],
            Some(r#"{"original_content":"one\n"}"#),
        );
        let diff_mode = DiffInput::for_tool(&tool, true).unwrap();
        assert_eq!(
            tags(&diff_mode.sections[0].diff_lines()),
            [ChangeTag::Equal, ChangeTag::Insert]
        );
        assert!(diff_mode.has_original);
        let file_mode = DiffInput::for_tool(&tool, false).unwrap();
        assert!(file_mode.has_original);
        assert_eq!(
            tags(&file_mode.sections[0].diff_lines()),
            [ChangeTag::Insert, ChangeTag::Insert]
        );
        assert_eq!(file_mode.sections[0].start_line, Some(1));
        assert_ne!(diff_mode.content_hash(), file_mode.content_hash());
    }

    #[test]
    fn nothing_to_show_means_no_input() {
        assert!(DiffInput::for_tool(&tool("edit", &[("path", "a.rs")], None), true).is_none());
        assert!(
            DiffInput::for_tool(&tool("write_file", &[("path", "a.rs")], None), true).is_none()
        );
        let delete = tool("delete_files", &[("paths", r#"["a.rs"]"#)], None);
        assert!(DiffInput::for_tool(&delete, true).is_none());
    }

    #[test]
    fn hash_and_size_follow_the_content() {
        let make = |new_text| {
            let tool = tool(
                "edit",
                &[
                    ("path", "a.rs"),
                    ("old_text", "abc"),
                    ("new_text", new_text),
                ],
                None,
            );
            DiffInput::for_tool(&tool, true).unwrap()
        };
        assert_eq!(make("abd").content_hash(), make("abd").content_hash());
        assert_ne!(make("abd").content_hash(), make("abe").content_hash());
        assert_eq!(make("abd").byte_len(), 6);
    }

    #[test]
    fn syntax_is_parsed_per_section_when_there_is_a_grammar() {
        let edit = |path| {
            let tool = tool(
                "edit",
                &[
                    ("path", path),
                    ("old_text", "fn a() {}"),
                    ("new_text", "fn b() {}"),
                ],
                None,
            );
            DiffInput::for_tool(&tool, true).unwrap()
        };
        let theme = HighlightTheme::default_dark();
        assert_eq!(
            edit("a.rs").parse_syntax(Some(&theme)).map(|s| s.len()),
            Some(1)
        );
        assert!(edit("notes.txt").parse_syntax(None).is_none());
    }
}
