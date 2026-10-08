//! Whole-line selection over [`DiffRows`](crate::tool_cards::diff_rows::DiffRows)
//! chunks, shared by the right panel's views. A view keeps one
//! [`LineSelection`] and builds each chunk's [`RowSelection`] with
//! [`row_selection`]; the element reports pointer positions as flat line
//! indices, which `key` ties to one file. A selection never spans files.

use crate::tool_cards::diff_rows::{EndCallback, LineCallback, RowSelection};
use gpui_kit::{App, Context, FocusHandle, Hsla, Window};

use std::ops::Range;
use std::rc::Rc;

/// A selection of flat lines `anchor..=head` (either may be the smaller) in
/// the file named by `key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected<K> {
    pub key: K,
    pub anchor: usize,
    pub head: usize,
}

impl<K> Selected<K> {
    /// Inclusive `(low, high)` line range.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// The selection state of one view.
#[derive(Debug)]
pub(crate) struct LineSelection<K> {
    selected: Option<Selected<K>>,
    /// Whether the pointer button is down for an in-progress drag.
    dragging: bool,
}

impl<K> Default for LineSelection<K> {
    fn default() -> Self {
        Self {
            selected: None,
            dragging: false,
        }
    }
}

impl<K: Clone + PartialEq> LineSelection<K> {
    pub fn get(&self) -> Option<&Selected<K>> {
        self.selected.as_ref()
    }

    pub fn clear(&mut self) {
        self.selected = None;
        self.dragging = false;
    }

    /// Drop the selection when `keep` rejects its file.
    pub fn retain(&mut self, keep: impl FnOnce(&K) -> bool) {
        if self.selected.as_ref().is_some_and(|s| !keep(&s.key)) {
            self.clear();
        }
    }

    /// Start a selection at `line` of `key`, replacing any previous one.
    /// With `extend` (shift held) and a selection in the same file, move its
    /// head instead.
    pub fn begin(&mut self, key: K, line: usize, extend: bool) {
        match &mut self.selected {
            Some(sel) if extend && sel.key == key => sel.head = line,
            _ => {
                self.selected = Some(Selected {
                    key,
                    anchor: line,
                    head: line,
                })
            }
        }
        self.dragging = true;
    }

    /// Move the head to `line` while a drag in `key` is in progress; returns
    /// whether anything changed.
    pub fn extend(&mut self, key: &K, line: usize) -> bool {
        if !self.dragging {
            return false;
        }
        match &mut self.selected {
            Some(sel) if &sel.key == key && sel.head != line => {
                sel.head = line;
                true
            }
            _ => false,
        }
    }

    /// End the drag; the selection itself stays.
    pub fn end(&mut self) {
        self.dragging = false;
    }

    /// The rows of a chunk (`row_count` rows starting at flat line
    /// `base_line` of `key`) to paint as selected, relative to the chunk.
    pub fn highlight(&self, key: &K, base_line: usize, row_count: usize) -> Range<usize> {
        let Some(sel) = self.selected.as_ref().filter(|s| &s.key == key) else {
            return 0..0;
        };
        let (lo, hi) = sel.range();
        let start = lo.max(base_line);
        let end = hi.min(base_line + row_count.saturating_sub(1));
        if row_count > 0 && start <= end {
            (start - base_line)..(end - base_line + 1)
        } else {
            0..0
        }
    }
}

/// A view that owns a [`LineSelection`].
pub(crate) trait SelectsLines: 'static + Sized {
    type Key: Clone + PartialEq + 'static;

    fn line_selection(&mut self) -> &mut LineSelection<Self::Key>;
}

/// How a chunk paints: the selected rows (from [`LineSelection::highlight`])
/// and the commented ones, both relative to the chunk.
pub(crate) struct ChunkMarks {
    pub highlight: Range<usize>,
    pub color: Hsla,
    pub marked: Vec<Range<usize>>,
    pub mark_color: Hsla,
}

/// The [`RowSelection`] for one chunk of `key`'s rows starting at flat line
/// `base_line`, wired to the view's [`LineSelection`]. Pressing focuses
/// `focus` (so copy shortcuts reach the view); shift-press extends the
/// selection.
pub(crate) fn row_selection<V: SelectsLines>(
    key: V::Key,
    base_line: usize,
    marks: ChunkMarks,
    focus: FocusHandle,
    cx: &Context<V>,
) -> RowSelection {
    // The element's closures are 'static: they reach the view through a weak
    // handle, so they never keep it alive.
    let entity = cx.entity().downgrade();
    let on_start: LineCallback = {
        let entity = entity.clone();
        let key = key.clone();
        Rc::new(move |line, window: &mut Window, cx: &mut App| {
            window.focus(&focus, cx);
            let extend = window.modifiers().shift;
            entity
                .update(cx, |view, cx| {
                    view.line_selection().begin(key.clone(), line, extend);
                    cx.notify();
                })
                .ok();
        })
    };
    let on_drag: LineCallback = {
        let entity = entity.clone();
        Rc::new(move |line, _window, cx| {
            entity
                .update(cx, |view, cx| {
                    if view.line_selection().extend(&key, line) {
                        cx.notify();
                    }
                })
                .ok();
        })
    };
    let on_end: EndCallback = Rc::new(move |_window, cx| {
        entity
            .update(cx, |view, cx| {
                view.line_selection().end();
                cx.notify();
            })
            .ok();
    });
    RowSelection {
        base_line,
        highlight: marks.highlight,
        color: marks.color,
        marked: marks.marked,
        mark_color: marks.mark_color,
        on_start,
        on_drag,
        on_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_extend_and_highlight() {
        let mut sel = LineSelection::default();
        sel.begin("a", 5, false);
        assert!(sel.extend(&"a", 8));
        assert!(!sel.extend(&"b", 9), "a drag never crosses files");
        sel.end();
        assert!(!sel.extend(&"a", 2), "no drag after release");
        assert_eq!(sel.get().unwrap().range(), (5, 8));
        // Chunk of rows 0..6 holds lines 5 and 6 of the selection.
        assert_eq!(sel.highlight(&"a", 0, 7), 5..7);
        assert_eq!(sel.highlight(&"a", 7, 10), 0..2);
        assert_eq!(sel.highlight(&"a", 9, 10), 0..0);
        assert_eq!(sel.highlight(&"b", 0, 10), 0..0);
    }

    #[test]
    fn shift_press_extends_in_the_same_file_only() {
        let mut sel = LineSelection::default();
        sel.begin("a", 5, false);
        sel.end();
        sel.begin("a", 9, true);
        assert_eq!(sel.get().unwrap().range(), (5, 9));
        sel.begin("b", 3, true);
        assert_eq!(sel.get().unwrap().range(), (3, 3));
        sel.retain(|k| *k == "a");
        assert!(sel.get().is_none());
    }
}
