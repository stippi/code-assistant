//! A choice of repository for a view of one repository at a time, when
//! the session's folder holds several: a search field over them. Shown in
//! place of the view until one is chosen, afterwards in a popover from its
//! toolbar. The pull request list and the Review tab use it.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    Context, Entity, EventEmitter, Render, ScrollHandle, SharedString, Subscription, Window, div,
    point, prelude::*, px, svg,
};

/// One repository to choose.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerRepo<K> {
    pub key: K,
    pub name: String,
    /// Muted after the name (owner, host).
    pub place: Option<String>,
    pub branch: Option<String>,
    /// At the end of the row ("3 changed").
    pub note: Option<String>,
    /// What else the search matches, besides the name and the branch.
    pub search: Vec<String>,
}

/// The repository chosen.
pub struct RepoPicked<K>(pub K);

/// The search matches the name, the branch or one of the repository's
/// search texts, ignoring case.
pub(crate) fn matches<K>(repo: &PickerRepo<K>, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || std::iter::once(repo.name.as_str())
            .chain(repo.branch.as_deref())
            .chain(repo.search.iter().map(String::as_str))
            .any(|text| text.to_lowercase().contains(&query))
}

pub struct RepoPicker<K> {
    repos: Vec<PickerRepo<K>>,
    selected: Option<K>,
    /// Shown above the search while nothing is chosen yet.
    prompt: &'static str,
    /// Prefix of element ids and debug selectors.
    prefix: &'static str,
    search: Entity<InputState>,
    query: String,
    list_scroll: ScrollHandle,
    _search: Subscription,
}

impl<K: 'static> EventEmitter<RepoPicked<K>> for RepoPicker<K> {}

impl<K: Clone + PartialEq + 'static> RepoPicker<K> {
    pub fn new(
        prompt: &'static str,
        prefix: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search repositories"));
        let subscription = cx.subscribe(
            &search,
            |this, search, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.query = search.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(first) = this.shown().next() {
                        cx.emit(RepoPicked(first.key.clone()));
                    }
                }
                _ => {}
            },
        );
        Self {
            repos: Vec::new(),
            selected: None,
            prompt,
            prefix,
            search,
            query: String::new(),
            list_scroll: ScrollHandle::new(),
            _search: subscription,
        }
    }

    pub fn set_repos(&mut self, repos: Vec<PickerRepo<K>>, cx: &mut Context<Self>) {
        self.repos = repos;
        cx.notify();
    }

    pub fn set_selected(&mut self, selected: Option<K>, cx: &mut Context<Self>) {
        self.selected = selected;
        cx.notify();
    }

    pub fn offers_a_choice(&self) -> bool {
        self.repos.len() > 1
    }

    /// Start a new search, ready for typing.
    pub fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // `set_value` reports no change, so the query does not follow by itself.
        self.query.clear();
        self.list_scroll.set_offset(point(px(0.), px(0.)));
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
    }

    fn shown(&self) -> impl Iterator<Item = &PickerRepo<K>> {
        self.repos.iter().filter(|repo| matches(repo, &self.query))
    }

    fn render_row(
        &self,
        ix: usize,
        repo: &PickerRepo<K>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<K> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let key = repo.key.clone();
        let selected = self.selected.as_ref() == Some(&key);
        let selector = format!("{}-repo-{}", self.prefix, repo.name);
        div()
            .id((SharedString::from(format!("{}-repo", self.prefix)), ix))
            .debug_selector(move || selector)
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(repo.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(muted)
                    .children(repo.place.clone()),
            )
            .when_some(repo.branch.clone(), |row, branch| {
                row.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(muted)
                        .child(
                            svg()
                                .flex_none()
                                .size(px(12.))
                                .path("icons/git_branch.svg")
                                .text_color(muted),
                        )
                        .child(div().min_w_0().truncate().child(branch)),
                )
            })
            .when_some(repo.note.clone(), |row, note| {
                row.child(div().flex_none().text_color(muted).child(note))
            })
            .when(selected, |row| {
                row.child(
                    svg()
                        .flex_none()
                        .size(px(13.))
                        .path("icons/check.svg")
                        .text_color(theme.foreground),
                )
            })
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(RepoPicked(key.clone()))))
    }
}

impl<K: Clone + PartialEq + 'static> Render for RepoPicker<K> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let shown: Vec<PickerRepo<K>> = self.shown().cloned().collect();
        // Until one is chosen this is the whole view, else a popover.
        let in_place = self.selected.is_none();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(in_place, |el| el.size_full().px_4().py_3())
            .when(!in_place, |el| el.w(px(320.)).max_h(px(360.)))
            .when(in_place, |el| {
                el.child(div().pb_1().text_sm().text_color(muted).child(self.prompt))
            })
            .child(Input::new(&self.search).with_size(Size::Small))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-repos", self.prefix)))
                            .flex_1()
                            .min_h_0()
                            .track_scroll(&self.list_scroll)
                            .overflow_y_scroll()
                            .when(shown.is_empty(), |el| {
                                el.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("No repository matches that."),
                                )
                            })
                            .children(
                                shown
                                    .iter()
                                    .enumerate()
                                    .map(|(ix, repo)| self.render_row(ix, repo, cx)),
                            ),
                    )
                    .vertical_scrollbar(&self.list_scroll),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_matches_name_branch_and_search_texts() {
        let repo = PickerRepo {
            key: 1,
            name: "tool".to_owned(),
            place: None,
            branch: Some("feature/x".into()),
            note: None,
            search: vec!["team/tool".into(), "checked-out-as".into()],
        };
        for query in ["", " TOOL ", "team/", "checked-out", "feature"] {
            assert!(matches(&repo, query), "{query}");
        }
        assert!(!matches(&repo, "other"));
    }
}
