//! The pull request tabs of the panel, after T3 Code's right panel tabs:
//! the list first, then one tab per opened pull request. Opening one that
//! is open shows it; closing the shown one shows its right neighbour,
//! else the new last, else the list; tabs move by drag and drop. Each tab
//! keeps its page (and so its scroll, sub-tab, drafts) while it is open.

/// Open tabs by pull request `K`, and which one shows (`None`: the list).
#[derive(Debug)]
pub struct Tabs<K, T> {
    items: Vec<(K, T)>,
    active: Option<usize>,
}

impl<K, T> Default for Tabs<K, T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            active: None,
        }
    }
}

impl<K: Clone + PartialEq, T> Tabs<K, T> {
    #[cfg(test)]
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.items.iter().map(|(key, _)| key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &T)> {
        self.items.iter().map(|(key, item)| (key, item))
    }

    pub fn position(&self, key: &K) -> Option<usize> {
        self.items.iter().position(|(k, _)| k == key)
    }

    /// The shown tab's index; `None` while the list shows.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub fn active_item(&self) -> Option<(&K, &T)> {
        let (key, item) = self.items.get(self.active?)?;
        Some((key, item))
    }

    /// Show the tab of `key`, made by `make` when it is not open yet
    /// (appended at the end).
    pub fn open(&mut self, key: K, make: impl FnOnce() -> T) -> usize {
        let ix = self.position(&key).unwrap_or_else(|| {
            self.items.push((key, make()));
            self.items.len() - 1
        });
        self.active = Some(ix);
        ix
    }

    /// Show the tab at `ix`, or the list (`None`).
    pub fn activate(&mut self, ix: Option<usize>) {
        self.active = ix.filter(|&ix| ix < self.items.len());
    }

    /// Close the tab at `ix`. When it showed, its right neighbour shows,
    /// else the new last tab, else the list.
    pub fn close(&mut self, ix: usize) -> Option<T> {
        if ix >= self.items.len() {
            return None;
        }
        let (_, item) = self.items.remove(ix);
        self.active = match self.active {
            Some(active) if active == ix => match self.items.len() {
                0 => None,
                len => Some(ix.min(len - 1)),
            },
            Some(active) if active > ix => Some(active - 1),
            other => other,
        };
        Some(item)
    }

    /// Close every tab `keep` rejects; the shown one stays shown if kept,
    /// else the one clicked (`fallback`) shows.
    fn close_where(&mut self, fallback: usize, mut keep: impl FnMut(usize) -> bool) -> Vec<T> {
        let shown = self.active.map(|ix| self.items[ix].0.clone());
        let clicked = self.items.get(fallback).map(|(key, _)| key.clone());
        let mut closed = Vec::new();
        let mut kept = Vec::new();
        for (ix, item) in std::mem::take(&mut self.items).into_iter().enumerate() {
            match keep(ix) {
                true => kept.push(item),
                false => closed.push(item.1),
            }
        }
        self.items = kept;
        // The list stays shown; a closed tab hands over to the clicked one.
        self.active = shown.and_then(|shown| {
            self.position(&shown)
                .or_else(|| clicked.and_then(|key| self.position(&key)))
                .or_else(|| self.items.len().checked_sub(1))
        });
        closed
    }

    pub fn close_others(&mut self, ix: usize) -> Vec<T> {
        self.close_where(ix, |other| other == ix)
    }

    pub fn close_to_the_right(&mut self, ix: usize) -> Vec<T> {
        self.close_where(ix, |other| other <= ix)
    }

    pub fn close_all(&mut self) -> Vec<T> {
        self.active = None;
        std::mem::take(&mut self.items)
            .into_iter()
            .map(|(_, item)| item)
            .collect()
    }

    /// Move the tab at `from` to `to`; the shown tab stays shown.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || from == to {
            return;
        }
        let to = to.min(self.items.len() - 1);
        let shown = self.active.map(|ix| self.items[ix].0.clone());
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.active = shown.and_then(|key| self.position(&key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(numbers: &[u64]) -> Tabs<u64, ()> {
        let mut tabs = Tabs::default();
        for &n in numbers {
            tabs.open(n, || ());
        }
        tabs
    }

    fn shape(tabs: &Tabs<u64, ()>) -> (Vec<u64>, Option<u64>) {
        (
            tabs.keys().copied().collect(),
            tabs.active_item().map(|(n, _)| *n),
        )
    }

    #[test]
    fn opening_an_open_pull_request_shows_its_tab() {
        let mut t = tabs(&[1, 2, 3]);
        assert_eq!(shape(&t), (vec![1, 2, 3], Some(3)));
        t.open(1, || unreachable!("open already"));
        assert_eq!(shape(&t), (vec![1, 2, 3], Some(1)));
        t.activate(None);
        assert_eq!(shape(&t).1, None, "the list shows");
    }

    #[test]
    fn closing_the_shown_tab_shows_its_right_neighbour_else_the_last() {
        let mut t = tabs(&[1, 2, 3]);
        t.activate(Some(1));
        t.close(1);
        assert_eq!(shape(&t), (vec![1, 3], Some(3)));
        t.close(1);
        assert_eq!(shape(&t), (vec![1], Some(1)));
        t.close(0);
        assert_eq!(shape(&t), (vec![], None));
        // Closing another tab keeps the shown one.
        let mut t = tabs(&[1, 2, 3]);
        t.close(0);
        assert_eq!(shape(&t), (vec![2, 3], Some(3)));
    }

    #[test]
    fn closing_others_or_to_the_right_keeps_what_showed() {
        let mut t = tabs(&[1, 2, 3, 4]);
        t.activate(Some(1));
        t.close_to_the_right(1);
        assert_eq!(shape(&t), (vec![1, 2], Some(2)));
        let mut t = tabs(&[1, 2, 3, 4]);
        t.close_others(2);
        assert_eq!(
            shape(&t),
            (vec![3], Some(3)),
            "the shown 4 went: the clicked one"
        );
        let mut t = tabs(&[1, 2]);
        t.activate(None);
        t.close_others(0);
        assert_eq!(shape(&t), (vec![1], None), "the list stays shown");
        assert_eq!(t.close_all().len(), 1);
        assert_eq!(shape(&t), (vec![], None));
    }

    #[test]
    fn tabs_move_and_the_shown_one_stays_shown() {
        let mut t = tabs(&[1, 2, 3]);
        t.move_tab(2, 0);
        assert_eq!(shape(&t), (vec![3, 1, 2], Some(3)));
        t.move_tab(0, 9);
        assert_eq!(shape(&t), (vec![1, 2, 3], Some(3)));
    }
}
