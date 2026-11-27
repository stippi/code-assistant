//! Which files show folded, and which a shift-click ticks. Ported from T3
//! Code's `pullRequestDiff.logic.ts`: a file's fold is kept relative to
//! the toolbar's collapse / expand all, so pages loaded later follow it.

use std::collections::HashSet;
use std::ops::Range;

#[derive(Debug, Default)]
pub struct Folds {
    /// Collapse all was pressed last (rather than expand all).
    all_folded: bool,
    /// Files folded the other way.
    toggled: HashSet<String>,
}

impl Folds {
    pub fn is_folded(&self, path: &str) -> bool {
        self.toggled.contains(path) != self.all_folded
    }

    pub fn toggle(&mut self, path: &str) {
        if !self.toggled.remove(path) {
            self.toggled.insert(path.to_owned());
        }
    }

    /// Fold or unfold `path` (ticking a file folds it, unticking unfolds).
    pub fn set(&mut self, path: &str, folded: bool) {
        if self.is_folded(path) != folded {
            self.toggle(path);
        }
    }

    /// Collapse or expand every file, now and as pages load.
    pub fn set_all(&mut self, folded: bool) {
        self.all_folded = folded;
        self.toggled.clear();
    }

    /// Whether all of `paths` are folded (and there are some).
    pub fn all_folded<'a>(&self, mut paths: impl Iterator<Item = &'a str>) -> bool {
        let mut any = false;
        let all = paths.all(|path| {
            any = true;
            self.is_folded(path)
        });
        any && all
    }
}

/// The files a click on `ix`'s checkbox ticks: with `anchor` (a shift-click
/// after a click on another file) everything between the two, else `ix`
/// alone.
pub fn viewed_range(ix: usize, anchor: Option<usize>) -> Range<usize> {
    match anchor {
        Some(anchor) => ix.min(anchor)..ix.max(anchor) + 1,
        None => ix..ix + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_are_kept_relative_to_collapse_all() {
        let mut folds = Folds::default();
        assert!(!folds.is_folded("a"));
        folds.toggle("a");
        assert!(folds.is_folded("a"));
        folds.set_all(true);
        assert!(folds.is_folded("a") && folds.is_folded("later"));
        folds.toggle("b");
        assert!(!folds.is_folded("b"));
        folds.set_all(false);
        assert!(!folds.is_folded("a") && !folds.is_folded("b"));
    }

    #[test]
    fn setting_a_fold_only_changes_it_when_it_differs() {
        let mut folds = Folds::default();
        folds.set("a", true);
        folds.set("a", true);
        assert!(folds.is_folded("a"));
        folds.set_all(true);
        folds.set("a", false);
        folds.set("a", false);
        assert!(!folds.is_folded("a"));
        assert!(folds.is_folded("b"));
    }

    #[test]
    fn all_folded_needs_files() {
        let mut folds = Folds::default();
        assert!(!folds.all_folded(std::iter::empty()));
        folds.set("a", true);
        assert!(folds.all_folded(["a"].into_iter()));
        assert!(!folds.all_folded(["a", "b"].into_iter()));
    }

    #[test]
    fn a_shift_click_ticks_the_range_from_the_last_click() {
        assert_eq!(viewed_range(3, None), 3..4);
        assert_eq!(viewed_range(3, Some(1)), 1..4);
        assert_eq!(viewed_range(1, Some(3)), 1..4);
    }
}
