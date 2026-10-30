//! The diff as GitHub sends it, page by page: each page's patch parsed
//! once, off the UI thread, into prepared file diffs in listing order
//! ([`parse_page`]); arrivals merged like T3 Code does it ([`arrival`]): a
//! re-read page that did not change keeps everything, a changed one
//! replaces itself and drops the pages after it (their cursors point into
//! the old diff).

use super::anchors::LineIndex;
use super::file_kind::is_test_file;
use super::file_order::order_files;
use crate::diff::prepare::PreparedFileDiff;
use git::ChangeStatus;
use github::model::DiffPage;
use gpui_kit::component::highlighter::HighlightTheme;
use similar::ChangeTag;
use std::hash::{Hash, Hasher};

/// One changed file of a page.
pub struct LoadedFile {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: ChangeStatus,
    pub diff: PreparedFileDiff,
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
/// prepared (and highlighted with `theme`). CPU-heavy: call off the UI
/// thread. Stamps are left for the view to give.
pub fn parse_page(page: &DiffPage, theme: &HighlightTheme) -> Vec<LoadedFile> {
    let mut files: Vec<Option<LoadedFile>> = git::parse_patch(&page.patch)
        .into_iter()
        .map(|patch| {
            let diff = PreparedFileDiff::from_patch(&patch, Some(theme));
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
            Some(LoadedFile {
                is_test: is_test_file(&patch.path),
                lines: LineIndex::new(&numbers),
                numbers,
                diff,
                additions,
                deletions,
                path: patch.path,
                orig_path: patch.orig_path,
                status: patch.status,
                stamp: 0,
                breaks: Vec::new(),
            })
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
        let files = parse_page(&page, &HighlightTheme::default_light());
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
