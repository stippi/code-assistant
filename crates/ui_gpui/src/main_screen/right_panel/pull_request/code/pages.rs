//! The diff as GitHub sends it, page by page: each page's patch parsed
//! once, off the UI thread, into prepared file diffs in listing order
//! ([`parse_page`]), unhighlighted (the view highlights file by file
//! afterwards); arrivals merged like T3 Code does it ([`arrival`]): a
//! re-read page that did not change keeps everything, a changed one
//! replaces itself and drops the pages after it (their cursors point into
//! the old diff).
//!
//! A file keeps its hunks as the patch has them and without whitespace
//! changes; what the list shows is shaped from one of them
//! ([`LoadedFile::reshape`]): the unchanged lines revealed around them
//! added, cut into chunks, side by side for the split layout.

use super::super::preferences::DiffLayout;
use super::anchors::LineIndex;
use super::context::{Revealed, ShownGap, shape};
use super::file_kind::is_test_file;
use super::file_order::order_files;
use crate::diff::model::DiffHunk;
use crate::diff::prepare::{CHUNK_MAX_LINES, PreparedFileDiff};
use crate::diff::rows::HScroll;
use crate::diff::split::SplitHunks;
use crate::diff::syntax::DiffSyntax;
use crate::diff::whitespace::hide_whitespace_changes;
use git::ChangeStatus;
use github::model::DiffPage;
use gpui_kit::SharedString;
use similar::ChangeTag;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// How the files are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffView {
    pub hide_whitespace: bool,
    pub layout: DiffLayout,
}

/// A file's new text, read to reveal unchanged lines, and both sides
/// parsed for highlighting.
pub struct FileText {
    pub new_lines: Vec<SharedString>,
    pub syntax: Option<Arc<DiffSyntax>>,
}

/// One changed file of a page.
pub struct LoadedFile {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: ChangeStatus,
    /// The hunks as the patch has them, and without whitespace changes.
    pub patch_hunks: Vec<DiffHunk>,
    pub quiet_hunks: Vec<DiffHunk>,
    /// What the list shows, shaped from one of the above.
    pub diff: PreparedFileDiff,
    /// The gaps between the shown hunks still hiding lines.
    pub gaps: Vec<ShownGap>,
    /// The shown hunks side by side, in the split layout.
    pub split: Option<SplitHunks>,
    /// Unchanged lines revealed, by gap.
    pub revealed: BTreeMap<usize, Revealed>,
    pub text: Option<FileText>,
    /// The patch hunks' syntax, once highlighted.
    pub hunk_syntax: Option<Arc<DiffSyntax>>,
    /// How far the rows scroll sideways when lines do not wrap.
    pub scroll: HScroll,
    /// Each row's change and line numbers (flat line indices).
    pub numbers: Vec<(ChangeTag, usize, usize)>,
    pub lines: LineIndex,
    pub is_test: bool,
    /// Lines added and removed; GitHub's count when it sent no hunks.
    pub additions: u64,
    pub deletions: u64,
    /// Names this file's rows: new per load and per cut of the chunks.
    pub stamp: u64,
    /// The flat lines the chunks are cut after (annotations go there).
    pub breaks: Vec<usize>,
    /// The syntax was looked for (there may be no grammar).
    pub highlighted: bool,
}

impl LoadedFile {
    /// Whether lines may follow the last hunk, and unchanged lines can be
    /// revealed (T3 Code: changed and renamed files).
    pub fn has_context(&self) -> bool {
        matches!(self.status, ChangeStatus::Modified | ChangeStatus::Renamed)
            && !self.diff.is_binary
            && !self.patch_hunks.is_empty()
    }

    /// Shape what the list shows from the hunks, the revealed lines and
    /// `view`; the chunks are cut anew, without breaks.
    pub fn reshape(&mut self, view: DiffView) {
        let base = match view.hide_whitespace {
            true => &self.quiet_hunks,
            false => &self.patch_hunks,
        };
        let new_lines = self.text.as_ref().map(|text| &text.new_lines[..]);
        let (hunks, gaps) = shape(base, &self.revealed, new_lines, self.has_context());
        self.diff.hunks = hunks;
        self.gaps = gaps;
        self.numbers = self.diff.row_numbers();
        self.lines = LineIndex::new(&self.numbers);
        // The patch's syntax fits as long as no line was added to a hunk;
        // revealing needs the file, which brings its own.
        let syntax = match &self.text {
            Some(text) => text.syntax.clone(),
            None => self.hunk_syntax.clone(),
        };
        self.diff.set_syntax(syntax);
        self.cut(&[], view.layout);
    }

    /// Cut the chunks so one ends after each flat line in `breaks`.
    pub fn cut(&mut self, breaks: &[usize], layout: DiffLayout) {
        self.diff.rechunk(breaks);
        self.breaks = breaks.to_vec();
        // A new or deleted file has one side only: it stays stacked.
        let two_sided = !matches!(self.status, ChangeStatus::Added | ChangeStatus::Deleted);
        self.split = (layout == DiffLayout::Split && two_sided)
            .then(|| SplitHunks::new(&self.diff.hunks, CHUNK_MAX_LINES, breaks));
    }
}

pub struct Page {
    /// The cursor this page was read with (`None` for the first).
    pub cursor: Option<String>,
    pub fingerprint: u64,
    pub next_cursor: Option<String>,
    /// GitHub withheld hunks in this page.
    pub truncated: bool,
    pub files: Vec<LoadedFile>,
}

/// What became of a page read with `cursor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// The held page is the same.
    Unchanged,
    /// Goes at this index; the pages from there on are dropped.
    Place(usize),
    /// Read for a diff that is gone.
    Stale,
}

/// Where a page read with `cursor` goes among `pages`.
pub fn arrival(pages: &[Page], cursor: Option<&str>, fingerprint: u64) -> Arrival {
    if let Some(ix) = pages.iter().position(|p| p.cursor.as_deref() == cursor) {
        return if pages[ix].fingerprint == fingerprint {
            Arrival::Unchanged
        } else {
            Arrival::Place(ix)
        };
    }
    let next = match pages.last() {
        Some(last) => last.next_cursor.as_deref(),
        None => None,
    };
    if next == cursor && (cursor.is_some() || pages.is_empty()) {
        Arrival::Place(pages.len())
    } else {
        Arrival::Stale
    }
}

/// What tells two reads of a page apart.
pub fn fingerprint(page: &DiffPage) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    page.patch.hash(&mut hasher);
    page.truncated.hash(&mut hasher);
    page.next_cursor.hash(&mut hasher);
    for stat in &page.omitted_file_stats {
        (&stat.path, stat.additions, stat.deletions).hash(&mut hasher);
    }
    hasher.finish()
}

/// The files of `page`, in the order they are listed, with their diffs
/// prepared but not highlighted, shaped for `view`. Call off the UI thread
/// (a few ms for a big page). Stamps are left for the view to give.
pub fn parse_page(page: &DiffPage, view: DiffView) -> Vec<LoadedFile> {
    let mut files: Vec<Option<LoadedFile>> = git::parse_patch(&page.patch)
        .into_iter()
        .map(|patch| {
            let diff = PreparedFileDiff::from_patch_plain(&patch);
            let numbers = diff.row_numbers();
            let (mut additions, mut deletions) = (diff.additions as u64, diff.deletions as u64);
            if additions == 0
                && deletions == 0
                && let Some(stat) = page
                    .omitted_file_stats
                    .iter()
                    .find(|stat| stat.path == patch.path)
            {
                (additions, deletions) = (stat.additions, stat.deletions);
            }
            let patch_hunks = diff.hunks.clone();
            let mut file = LoadedFile {
                is_test: is_test_file(&patch.path),
                lines: LineIndex::new(&numbers),
                numbers,
                quiet_hunks: hide_whitespace_changes(&patch_hunks),
                patch_hunks,
                diff,
                gaps: Vec::new(),
                split: None,
                revealed: BTreeMap::new(),
                text: None,
                hunk_syntax: None,
                scroll: HScroll::default(),
                additions,
                deletions,
                path: patch.path,
                orig_path: patch.orig_path,
                status: patch.status,
                stamp: 0,
                breaks: Vec::new(),
                highlighted: false,
            };
            file.reshape(view);
            Some(file)
        })
        .collect();
    let paths: Vec<String> = files
        .iter()
        .flatten()
        .map(|file| file.path.clone())
        .collect();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let order = order_files(&paths, |ix| {
        files[ix]
            .as_ref()
            .map(|file| {
                file.diff
                    .hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .filter(|line| line.tag != ChangeTag::Equal)
                    .map(|line| line.text.as_ref())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    order
        .into_iter()
        .filter_map(|ix| files[ix].take())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(cursor: Option<&str>, fingerprint: u64, next: Option<&str>) -> Page {
        Page {
            cursor: cursor.map(str::to_owned),
            fingerprint,
            next_cursor: next.map(str::to_owned),
            truncated: false,
            files: Vec::new(),
        }
    }

    #[test]
    fn pages_append_in_cursor_order() {
        assert_eq!(arrival(&[], None, 1), Arrival::Place(0));
        assert_eq!(arrival(&[], Some("2"), 1), Arrival::Stale);
        let pages = [page(None, 1, Some("2"))];
        assert_eq!(arrival(&pages, Some("2"), 5), Arrival::Place(1));
        assert_eq!(arrival(&pages, Some("3"), 5), Arrival::Stale);
        let last = [page(None, 1, None)];
        assert_eq!(arrival(&last, Some("2"), 5), Arrival::Stale);
    }

    #[test]
    fn a_reread_page_is_kept_or_replaces_the_rest() {
        let pages = [page(None, 1, Some("2")), page(Some("2"), 2, None)];
        assert_eq!(arrival(&pages, None, 1), Arrival::Unchanged);
        assert_eq!(arrival(&pages, None, 9), Arrival::Place(0));
        assert_eq!(arrival(&pages, Some("2"), 9), Arrival::Place(1));
    }

    const VIEW: DiffView = DiffView {
        hide_whitespace: false,
        layout: DiffLayout::Stacked,
    };

    const PATCH: &str = "\
diff --git a/src/b.ts b/src/b.ts
--- a/src/b.ts
+++ b/src/b.ts
@@ -1,1 +1,1 @@
-old
+new
diff --git a/src/a.ts b/src/a.ts
new file mode 100644
--- /dev/null
+++ b/src/a.ts
@@ -0,0 +1,1 @@
+import { x } from \"./z\";
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
diff --git a/src/z.ts b/src/z.ts
--- a/src/z.ts
+++ b/src/z.ts
@@ -3,1 +3,2 @@
 ctx
+more
";

    #[test]
    fn a_page_parses_into_ordered_files_with_their_counts() {
        let page = DiffPage {
            patch: PATCH.into(),
            truncated: true,
            next_cursor: None,
            omitted_file_stats: vec![github::model::OmittedFileStat {
                path: "img.png".into(),
                additions: 3,
                deletions: 1,
            }],
        };
        let files = parse_page(&page, VIEW);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        // `a.ts` imports `z.ts`; everything else in path order.
        assert_eq!(paths, ["img.png", "src/b.ts", "src/z.ts", "src/a.ts"]);
        let b = &files[1];
        assert_eq!((b.additions, b.deletions), (1, 1));
        assert_eq!(b.status, ChangeStatus::Modified);
        assert_eq!(files[3].status, ChangeStatus::Added);
        let png = &files[0];
        assert!(png.diff.is_binary);
        assert_eq!((png.additions, png.deletions), (3, 1));
        // The line index points at rows: `more` is new line 4, row 1.
        let z = &files[2];
        assert_eq!(
            z.lines.row(super::super::anchors::LineRef {
                side: github::model::DiffSide::Right,
                line: 4
            }),
            Some(1)
        );
        assert_ne!(fingerprint(&page), {
            let mut other = page.clone();
            other.truncated = false;
            fingerprint(&other)
        });
    }
}
