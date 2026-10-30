//! A file's diff, made ready to render.

use super::chunks::{ChunkedHunks, chunk_hunks};
use super::model::{
    DiffHunk, compute_diff_hunks, hunks_from_patch, normalize_for_diff, single_sided_hunk,
};
use super::syntax::DiffSyntax;
use gpui_kit::component::highlighter::HighlightTheme;
use similar::ChangeTag;
use std::sync::Arc;

/// Context lines around each computed hunk (matches `git diff`'s default).
const HUNK_CONTEXT_LINES: usize = 3;

/// Upper bound for the lines of one list item. A hunk above it (a newly
/// added file is a single hunk) is split, so a virtualized list only builds
/// what is in view.
pub const CHUNK_MAX_LINES: usize = 40;

/// A file diff reduced to renderable hunks. The expensive parts (line diff,
/// syntax parse, line styles) run once on a background thread — the UI only
/// builds elements from the prepared hunks, so element counts scale with
/// changed lines and the UI thread never runs a Myers diff. `chunked` cuts
/// the hunks into the items of a virtualized list.
#[derive(Debug, Clone)]
pub struct PreparedFileDiff {
    pub is_binary: bool,
    pub too_large: bool,
    pub hunks: Vec<DiffHunk>,
    pub chunked: ChunkedHunks,
    /// Both sides parsed for syntax highlighting; `None` without a grammar.
    pub syntax: Option<Arc<DiffSyntax>>,
    pub additions: usize,
    pub deletions: usize,
}

impl PreparedFileDiff {
    /// From the two whole sides of a local diff: hunks with context lines,
    /// both files parsed for highlighting. With `theme`, the line styles are
    /// computed right away too. CPU-heavy — call on a background thread.
    pub fn from_content(
        path: &str,
        diff: &git::FileDiffContent,
        theme: Option<&HighlightTheme>,
    ) -> Self {
        if diff.is_binary || diff.too_large {
            return Self::placeholder(diff.is_binary, diff.too_large);
        }
        let old = diff.old_text.as_deref().filter(|t| !t.is_empty());
        let new = diff.new_text.as_deref().filter(|t| !t.is_empty());
        let hunks = match (old, new) {
            (None, None) => Vec::new(),
            (Some(old), Some(new)) => compute_diff_hunks(old, new, HUNK_CONTEXT_LINES),
            // Pure add / pure delete: the whole file is the hunk.
            (None, Some(new)) => single_sided_hunk(new, ChangeTag::Insert),
            (Some(old), None) => single_sided_hunk(old, ChangeTag::Delete),
        };
        Self::from_hunks(hunks, theme, |_| {
            // The same normalization the line diff applies, so lines agree.
            DiffSyntax::parse(
                path,
                old.map(normalize_for_diff).as_deref(),
                new.map(normalize_for_diff).as_deref(),
            )
        })
    }

    /// From one file of a parsed patch, which has only the hunks: each
    /// hunk's sides are parsed on their own. With `theme`, the line styles
    /// are computed right away too. CPU-heavy — call on a background thread.
    pub fn from_patch(file: &git::FilePatch, theme: Option<&HighlightTheme>) -> Self {
        if file.is_binary {
            return Self::placeholder(true, false);
        }
        Self::from_hunks(hunks_from_patch(&file.hunks), theme, |hunks| {
            DiffSyntax::parse_hunks(&file.path, hunks)
        })
    }

    /// [`Self::from_patch`] without the syntax: cheap enough to show the
    /// rows at once; [`highlight_patch`] adds it later.
    pub fn from_patch_plain(file: &git::FilePatch) -> Self {
        if file.is_binary {
            return Self::placeholder(true, false);
        }
        Self::from_hunks(hunks_from_patch(&file.hunks), None, |_| None)
    }

    fn placeholder(is_binary: bool, too_large: bool) -> Self {
        Self {
            is_binary,
            too_large,
            hunks: Vec::new(),
            chunked: ChunkedHunks::default(),
            syntax: None,
            additions: 0,
            deletions: 0,
        }
    }

    fn from_hunks(
        hunks: Vec<DiffHunk>,
        theme: Option<&HighlightTheme>,
        parse: impl FnOnce(&[DiffHunk]) -> Option<DiffSyntax>,
    ) -> Self {
        let count = |tag| {
            hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.tag == tag)
                .count()
        };
        // Nothing to show means nothing to parse.
        let syntax = (!hunks.is_empty()).then(|| parse(&hunks)).flatten();
        if let (Some(syntax), Some(theme)) = (&syntax, theme) {
            syntax.prime(theme);
        }
        Self {
            is_binary: false,
            too_large: false,
            chunked: chunk_hunks(&hunks, CHUNK_MAX_LINES, &[]),
            syntax: syntax.map(Arc::new),
            additions: count(ChangeTag::Insert),
            deletions: count(ChangeTag::Delete),
            hunks,
        }
    }

    /// Show the rows highlighted with `syntax` (see [`highlight_patch`]).
    pub fn set_syntax(&mut self, syntax: Option<Arc<DiffSyntax>>) {
        self.syntax = syntax;
    }

    /// Cut the hunks into chunks again, so one ends after each flat line
    /// index in `breaks` (see [`chunk_hunks`]).
    pub fn rechunk(&mut self, breaks: &[usize]) {
        self.chunked = chunk_hunks(&self.hunks, CHUNK_MAX_LINES, breaks);
    }

    /// One row's change tag and its 1-based line numbers in the old and new
    /// file, for every row of the hunks in render order (flat line index).
    /// A row missing on one side has the number of that side's next line.
    pub fn row_numbers(&self) -> Vec<(ChangeTag, usize, usize)> {
        let mut rows = Vec::new();
        for hunk in &self.hunks {
            let (mut old, mut new) = (hunk.old_start, hunk.new_start);
            for line in &hunk.lines {
                rows.push((line.tag, old, new));
                if line.tag != ChangeTag::Insert {
                    old += 1;
                }
                if line.tag != ChangeTag::Delete {
                    new += 1;
                }
            }
        }
        rows
    }
}

/// The syntax of a patch file's hunks (prepared by
/// [`PreparedFileDiff::from_patch_plain`]), with the line styles for
/// `theme` computed. CPU-heavy — call on a background thread.
pub fn highlight_patch(
    path: &str,
    hunks: &[DiffHunk],
    theme: &HighlightTheme,
) -> Option<Arc<DiffSyntax>> {
    let syntax = DiffSyntax::parse_hunks(path, hunks)?;
    syntax.prime(theme);
    Some(Arc::new(syntax))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ impl Foo {
     fn keep() {}
-    fn old() {}
+    fn new() {}
+    fn more() {}
     fn tail() {}
@@ -80,2 +81,1 @@
-fn gone() {}
 fn done() {}
";

    #[test]
    fn from_patch_keeps_positions_and_counts() {
        let files = git::parse_patch(PATCH);
        let theme = HighlightTheme::default_dark();
        let prepared = PreparedFileDiff::from_patch(&files[0], Some(&theme));
        assert_eq!(prepared.hunks.len(), 2);
        assert_eq!(prepared.hunks[0].header.as_deref(), Some("impl Foo {"));
        assert_eq!(
            (prepared.hunks[1].old_start, prepared.hunks[1].new_start),
            (80, 81)
        );
        assert_eq!((prepared.additions, prepared.deletions), (2, 2));
        assert_eq!(prepared.chunked.chunks.len(), 2);
        assert_eq!(prepared.chunked.chunks[1].flat_start, 5);
        let syntax = prepared.syntax.as_ref().expect("rust has a grammar");
        assert_eq!(syntax.origin(), crate::diff::syntax::SyntaxOrigin::PerHunk);
        // Hunk 1's first new line is `fn done`, line 1 of its own text.
        let styles = syntax.line_styles(ChangeTag::Equal, 1, 1, "fn done() {}", &theme);
        assert!(!styles.is_empty());
    }

    #[test]
    fn a_plain_patch_diff_is_highlighted_later() {
        let files = git::parse_patch(PATCH);
        let theme = HighlightTheme::default_dark();
        let mut prepared = PreparedFileDiff::from_patch_plain(&files[0]);
        assert!(prepared.syntax.is_none());
        assert_eq!((prepared.additions, prepared.deletions), (2, 2));
        let syntax = highlight_patch(&files[0].path, &prepared.hunks, &theme);
        prepared.set_syntax(syntax);
        let syntax = prepared.syntax.as_ref().expect("rust has a grammar");
        assert!(
            !syntax
                .line_styles(ChangeTag::Equal, 1, 1, "fn done() {}", &theme)
                .is_empty()
        );
        assert!(highlight_patch("notes.unknown", &prepared.hunks, &theme).is_none());
    }

    #[test]
    fn binary_patches_and_contents_are_placeholders() {
        let files = git::parse_patch(
            "diff --git a/a.png b/a.png\nBinary files a/a.png and b/a.png differ\n",
        );
        let prepared = PreparedFileDiff::from_patch(&files[0], None);
        assert!(prepared.is_binary);
        assert!(prepared.hunks.is_empty() && prepared.syntax.is_none());

        let prepared = PreparedFileDiff::from_content(
            "a.rs",
            &git::FileDiffContent {
                old_text: None,
                new_text: None,
                is_binary: false,
                too_large: true,
            },
            None,
        );
        assert!(prepared.too_large);
        assert!(prepared.chunked.chunks.is_empty());
    }

    #[test]
    fn from_content_parses_whole_files() {
        let theme = HighlightTheme::default_dark();
        let prepared = PreparedFileDiff::from_content(
            "a.rs",
            &git::FileDiffContent {
                old_text: Some("fn a() {}\nfn b() {}\n".into()),
                new_text: Some("fn a() {}\nfn c() {}\n".into()),
                is_binary: false,
                too_large: false,
            },
            Some(&theme),
        );
        assert_eq!((prepared.additions, prepared.deletions), (1, 1));
        let syntax = prepared.syntax.as_ref().unwrap();
        assert_eq!(
            syntax.origin(),
            crate::diff::syntax::SyntaxOrigin::WholeFile
        );
        // Whole-file line numbers: `fn c` is new line 2.
        assert!(
            !syntax
                .line_styles(ChangeTag::Insert, 0, 2, "fn c() {}", &theme)
                .is_empty()
        );
    }

    #[test]
    fn rechunk_ends_chunks_after_break_points() {
        let files = git::parse_patch(PATCH);
        let mut prepared = PreparedFileDiff::from_patch(&files[0], None);
        prepared.rechunk(&[1]);
        let starts: Vec<_> = prepared
            .chunked
            .chunks
            .iter()
            .map(|c| c.flat_start)
            .collect();
        assert_eq!(starts, [0, 2, 5]);
    }

    #[test]
    fn row_numbers_count_each_side() {
        let files = git::parse_patch(PATCH);
        let prepared = PreparedFileDiff::from_patch(&files[0], None);
        let numbers = prepared.row_numbers();
        assert_eq!(numbers.len(), 7);
        assert_eq!(numbers[1], (ChangeTag::Delete, 11, 11));
        assert_eq!(numbers[3], (ChangeTag::Insert, 12, 12));
        assert_eq!(numbers[4], (ChangeTag::Equal, 12, 13));
        assert_eq!(numbers[5], (ChangeTag::Delete, 80, 81));
        assert_eq!(numbers[6], (ChangeTag::Equal, 81, 81));
    }
}
