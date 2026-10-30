//! The unchanged lines between a file's hunks, and showing them. GitHub's
//! patch leaves them out; the gaps' sizes follow from the hunk headers,
//! except past the last hunk, which is unknown until the file is read
//! ("More unchanged context may be available"). Like T3 Code (`@pierre/
//! diffs`), a click reveals 100 lines at a time from the side of a gap it
//! names, or the whole gap; the lines come from the new file's text, read
//! on the first click (`pull_request_file_contents`). A gap shown whole
//! joins the hunks around it.

use crate::diff::model::{DiffHunk, DiffLine};
use gpui_kit::SharedString;
use similar::ChangeTag;
use std::collections::BTreeMap;

/// Lines one click reveals.
pub const EXPAND_STEP: usize = 100;

/// Lines shown of one gap: from its top (below the hunk above) and from
/// its bottom (above the hunk below). Either may exceed the gap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revealed {
    pub top: usize,
    pub bottom: usize,
}

/// What a click on a gap asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    /// The lines below the hunk above ("expand down").
    Down,
    /// The lines above the hunk below ("expand up").
    Up,
    All,
}

impl Revealed {
    pub fn apply(&mut self, reveal: Reveal) {
        match reveal {
            Reveal::Down => self.top = self.top.saturating_add(EXPAND_STEP),
            Reveal::Up => self.bottom = self.bottom.saturating_add(EXPAND_STEP),
            Reveal::All => (self.top, self.bottom) = (usize::MAX, usize::MAX),
        }
    }
}

/// A gap that still hides lines (or names a section).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownGap {
    /// Its index among the patch's gaps: before the patch's hunk `gap`, or
    /// after the last one (`gap` = the patch's hunk count).
    pub gap: usize,
    /// The shown hunk it comes before; the shown hunk count after the last.
    pub before: usize,
    /// Unchanged lines hidden; `None` past the last hunk until the file's
    /// length is known.
    pub hidden: Option<usize>,
    /// The `@@` section of the hunk below.
    pub section: Option<SharedString>,
}

/// The buttons of a gap, after T3 Code: one that shows a small gap whole;
/// for a larger one, the directions there are lines to come from, and
/// "Expand all" between hunks.
pub fn gap_reveals(gap: &ShownGap, patch_hunks: usize) -> Vec<Reveal> {
    let leading = gap.gap == 0;
    let trailing = gap.gap == patch_hunks;
    match gap.hidden {
        Some(0) => Vec::new(),
        Some(hidden) if hidden <= EXPAND_STEP => vec![Reveal::All],
        _ if leading => vec![Reveal::Up],
        _ if trailing => vec![Reveal::Down],
        _ => vec![Reveal::Down, Reveal::Up, Reveal::All],
    }
}

/// A hunk's first line and the line after it, on the old and new side.
/// A side without lines starts after the line its header names.
fn span(hunk: &DiffHunk) -> ((usize, usize), (usize, usize)) {
    let count = |skip: ChangeTag| hunk.lines.iter().filter(|l| l.tag != skip).count();
    let (old, new) = (count(ChangeTag::Insert), count(ChangeTag::Delete));
    let old_first = hunk.old_start + usize::from(old == 0);
    let new_first = hunk.new_start + usize::from(new == 0);
    ((old_first, old_first + old), (new_first, new_first + new))
}

/// The file's hunks with the revealed lines added, and the gaps that
/// still hide some. `new_lines` is the new file's text by line, once read
/// (nothing is revealed before); `tail` whether the file may go on past
/// its last hunk (a modified or renamed file).
pub fn shape(
    hunks: &[DiffHunk],
    revealed: &BTreeMap<usize, Revealed>,
    new_lines: Option<&[SharedString]>,
    tail: bool,
) -> (Vec<DiffHunk>, Vec<ShownGap>) {
    let mut shown: Vec<DiffHunk> = Vec::with_capacity(hunks.len());
    let mut gaps = Vec::new();
    let context = |from: usize, count: usize| -> Vec<DiffLine> {
        let lines = new_lines.unwrap_or_default();
        (from..from + count)
            .map(|line| {
                let text = lines.get(line - 1).map_or("", |t| t.as_ref());
                DiffLine::plain(ChangeTag::Equal, text)
            })
            .collect()
    };
    // The lines after the previous hunk, old and new side.
    let (mut old_next, mut new_next) = (1, 1);
    for (ix, hunk) in hunks.iter().enumerate() {
        let ((old_first, old_after), (new_first, new_after)) = span(hunk);
        let size = old_first.saturating_sub(old_next);
        let wanted = revealed.get(&ix).copied().unwrap_or_default();
        let (top, bottom) = match new_lines {
            None => (0, 0),
            // Above the first hunk only lines next to it show.
            Some(_) if ix == 0 => (0, wanted.bottom.min(size)),
            Some(_) => {
                let top = wanted.top.min(size);
                (top, wanted.bottom.min(size - top))
            }
        };
        if let Some(last) = shown.last_mut() {
            last.lines.extend(context(new_next, top));
        }
        let hidden = size - top - bottom;
        if ix > 0 && size > 0 && hidden == 0 {
            // Shown whole: one hunk with the one above.
            let last = shown.last_mut().expect("a hunk above");
            last.lines.extend(context(new_next + top, bottom));
            last.lines.extend(hunk.lines.iter().cloned());
        } else {
            // A section names the gap until its lines show instead.
            if hidden > 0 || (size == 0 && hunk.header.is_some()) {
                gaps.push(ShownGap {
                    gap: ix,
                    before: shown.len(),
                    hidden: Some(hidden),
                    section: hunk.header.clone(),
                });
            }
            let mut lines = context(new_first - bottom, bottom);
            lines.extend(hunk.lines.iter().cloned());
            shown.push(DiffHunk {
                old_start: if bottom > 0 {
                    old_first - bottom
                } else {
                    hunk.old_start
                },
                new_start: if bottom > 0 {
                    new_first - bottom
                } else {
                    hunk.new_start
                },
                header: hunk.header.clone(),
                lines,
            });
        }
        (old_next, new_next) = (old_after, new_after);
    }
    if tail && let Some(last) = shown.last_mut() {
        let wanted = revealed.get(&hunks.len()).copied().unwrap_or_default();
        let size = new_lines.map(|lines| (lines.len() + 1).saturating_sub(new_next));
        let top = size.map_or(0, |size| wanted.top.min(size));
        last.lines.extend(context(new_next, top));
        let hidden = size.map(|size| size - top);
        if hidden != Some(0) {
            gaps.push(ShownGap {
                gap: hunks.len(),
                before: shown.len(),
                hidden,
                section: None,
            });
        }
    }
    (shown, gaps)
}

/// A file's text by line, as `shape` takes it.
pub fn lines_of(text: &str) -> Vec<SharedString> {
    text.lines()
        .map(|line| SharedString::from(line.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChangeTag::*;

    fn hunk(
        old_start: usize,
        new_start: usize,
        header: Option<&str>,
        tags: &[ChangeTag],
    ) -> DiffHunk {
        DiffHunk {
            old_start,
            new_start,
            header: header.map(SharedString::from),
            lines: tags
                .iter()
                .map(|&tag| DiffLine::plain(tag, "changed"))
                .collect(),
        }
    }

    /// Lines 1..=n of the new file, `"line k"`.
    fn file(n: usize) -> Vec<SharedString> {
        (1..=n).map(|k| format!("line {k}").into()).collect()
    }

    fn texts(hunk: &DiffHunk) -> Vec<String> {
        hunk.lines.iter().map(|l| l.text.to_string()).collect()
    }

    /// Hunk 0 covers old 11..=13 (new 11..=14), hunk 1 old 31..=31.
    fn patch() -> Vec<DiffHunk> {
        vec![
            hunk(11, 11, None, &[Equal, Delete, Insert, Insert, Equal]),
            hunk(31, 32, Some("fn main() {"), &[Equal]),
        ]
    }

    #[test]
    fn gaps_are_sized_from_the_headers_and_the_tail_is_unknown() {
        let (hunks, gaps) = shape(&patch(), &BTreeMap::new(), None, true);
        assert_eq!(hunks.len(), 2);
        let summary: Vec<_> = gaps.iter().map(|g| (g.gap, g.before, g.hidden)).collect();
        assert_eq!(summary, [(0, 0, Some(10)), (1, 1, Some(17)), (2, 2, None)]);
        assert_eq!(gaps[1].section.as_deref(), Some("fn main() {"));
        // Added or deleted files have no tail.
        let (_, gaps) = shape(&patch(), &BTreeMap::new(), None, false);
        assert_eq!(gaps.len(), 2);
    }

    #[test]
    fn revealing_needs_the_file_and_takes_lines_from_both_ends() {
        let revealed = BTreeMap::from([
            (0, Revealed { top: 5, bottom: 3 }),
            (1, Revealed { top: 2, bottom: 4 }),
        ]);
        let (unread, _) = shape(&patch(), &revealed, None, true);
        assert_eq!(unread[0].lines.len(), 5, "nothing without the file");

        let lines = file(40);
        let (hunks, gaps) = shape(&patch(), &revealed, Some(&lines), true);
        // Above the first hunk only the lines next to it: new 8..=10.
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (8, 8));
        assert_eq!(&texts(&hunks[0])[..3], ["line 8", "line 9", "line 10"]);
        // Below hunk 0: new 15, 16; above hunk 1: new 28..=31.
        assert_eq!(&texts(&hunks[0])[8..], ["line 15", "line 16"]);
        assert_eq!((hunks[1].old_start, hunks[1].new_start), (27, 28));
        assert_eq!(
            &texts(&hunks[1])[..4],
            ["line 28", "line 29", "line 30", "line 31"]
        );
        let hidden: Vec<_> = gaps.iter().map(|g| g.hidden).collect();
        // The tail is known now: new 33..=40.
        assert_eq!(hidden, [Some(7), Some(11), Some(8)]);
    }

    #[test]
    fn a_gap_shown_whole_joins_its_hunks_and_the_tail_ends() {
        let lines = file(33);
        let revealed = BTreeMap::from([
            (
                1,
                Revealed {
                    top: usize::MAX,
                    bottom: 0,
                },
            ),
            (
                2,
                Revealed {
                    top: usize::MAX,
                    bottom: 0,
                },
            ),
        ]);
        let (hunks, gaps) = shape(&patch(), &revealed, Some(&lines), true);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines.len(), 5 + 17 + 1 + 1);
        assert_eq!(texts(&hunks[0]).last().unwrap(), "line 33");
        let left: Vec<_> = gaps.iter().map(|g| (g.gap, g.hidden)).collect();
        assert_eq!(left, [(0, Some(10))]);
    }

    #[test]
    fn small_gaps_show_whole_with_one_click_larger_ones_by_side() {
        let gap = |gap, hidden| ShownGap {
            gap,
            before: gap,
            hidden,
            section: None,
        };
        use Reveal::*;
        assert_eq!(gap_reveals(&gap(1, Some(30)), 3), [All]);
        assert_eq!(gap_reveals(&gap(1, Some(300)), 3), [Down, Up, All]);
        assert_eq!(gap_reveals(&gap(0, Some(300)), 3), [Up]);
        assert_eq!(gap_reveals(&gap(3, Some(300)), 3), [Down]);
        assert_eq!(gap_reveals(&gap(3, None), 3), [Down]);
        assert!(gap_reveals(&gap(1, Some(0)), 3).is_empty());

        let mut revealed = Revealed::default();
        revealed.apply(Down);
        revealed.apply(Up);
        revealed.apply(Up);
        assert_eq!(
            revealed,
            Revealed {
                top: 100,
                bottom: 200
            }
        );
    }

    #[test]
    fn a_hunk_without_old_lines_starts_after_its_header_line() {
        // `@@ -7,0 +8,2 @@`: inserted after old line 7.
        let hunks = [hunk(7, 8, None, &[Insert, Insert])];
        let (_, gaps) = shape(&hunks, &BTreeMap::new(), None, false);
        assert_eq!(gaps[0].hidden, Some(7));
    }
}
