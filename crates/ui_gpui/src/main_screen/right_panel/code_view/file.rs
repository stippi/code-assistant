//! One changed file as the code views show it. A file keeps its hunks as
//! they came (a patch's, or computed from both sides of a local diff) and
//! without whitespace changes; what the list shows is shaped from one of
//! them ([`LoadedFile::reshape`]): the unchanged lines revealed around them
//! added, cut into chunks, side by side for the split layout.

use super::anchors::LineIndex;
use super::context::{Revealed, ShownGap, shape};
use super::preferences::DiffLayout;
use crate::diff::model::DiffHunk;
use crate::diff::prepare::{CHUNK_MAX_LINES, PreparedFileDiff};
use crate::diff::rows::HScroll;
use crate::diff::split::SplitHunks;
use crate::diff::syntax::DiffSyntax;
use git::ChangeStatus;
use gpui_kit::SharedString;
use similar::ChangeTag;
use std::collections::BTreeMap;
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
