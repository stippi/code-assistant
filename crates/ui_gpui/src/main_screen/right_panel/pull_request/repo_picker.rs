//! Which repository the pull request list shows, for a session whose folder
//! holds several checkouts: a search field over them. Shown in place of the
//! list until one is chosen, afterwards in a popover from its toolbar.

use code_assistant_core::pull_requests::{RepoCheckout, SessionCheckouts};
use github::RepoRef;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    Context, Entity, EventEmitter, Render, Subscription, Window, div, prelude::*, px, svg,
};

/// The repository to list instead.
pub struct RepoPicked(pub RepoRef);

/// The repository a session lists at first: the one it listed last, else
/// the one of its folder itself (as without any others), else the one last
/// chosen in the folder, else the only one. `None` leaves the choice open.
pub fn first_focus<'a>(
    found: &'a SessionCheckouts,
    listed_last: Option<&RepoRef>,
    recent: &[RepoRef],
) -> Option<&'a RepoCheckout> {
    let checkouts = &found.checkouts;
    let of = |repo: &RepoRef| checkouts.iter().find(|checkout| checkout.repo == *repo);
    listed_last
        .and_then(of)
        .or_else(|| checkouts.iter().find(|c| c.path == found.directory))
        .or_else(|| recent.iter().find_map(of))
        .or(match checkouts.as_slice() {
            [only] => Some(only),
            _ => None,
        })
}

/// The ones chosen recently first, the others as they came.
pub fn recent_first(mut checkouts: Vec<RepoCheckout>, recent: &[RepoRef]) -> Vec<RepoCheckout> {
    checkouts.sort_by_key(|checkout| {
        recent
            .iter()
            .position(|repo| *repo == checkout.repo)
            .unwrap_or(recent.len())
    });
    checkouts
}

/// The search matches the repository's or the directory's name, its owner
/// or the branch, ignoring case.
fn matches(checkout: &RepoCheckout, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    let directory = checkout.path.file_name().map(|name| name.to_string_lossy());
    query.is_empty()
        || [
            Some(checkout.repo.full_name().as_str()),
            directory.as_deref(),
            checkout.branch.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(&query))
}

pub struct RepoPicker {
    checkouts: Vec<RepoCheckout>,
    selected: Option<RepoRef>,
    search: Entity<InputState>,
    query: String,
    _search: Subscription,
}

impl EventEmitter<RepoPicked> for RepoPicker {}

impl RepoPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                        cx.emit(RepoPicked(first.repo.clone()));
                    }
                }
                _ => {}
            },
        );
        Self {
            checkouts: Vec::new(),
            selected: None,
            search,
            query: String::new(),
            _search: subscription,
        }
    }

    pub fn set_checkouts(&mut self, checkouts: Vec<RepoCheckout>, cx: &mut Context<Self>) {
        self.checkouts = checkouts;
        cx.notify();
    }

    pub fn set_selected(&mut self, selected: Option<RepoRef>, cx: &mut Context<Self>) {
        self.selected = selected;
        cx.notify();
    }

    pub fn offers_a_choice(&self) -> bool {
        self.checkouts.len() > 1
    }

    /// Start a new search, ready for typing.
    pub fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
    }

    fn shown(&self) -> impl Iterator<Item = &RepoCheckout> {
        self.checkouts
            .iter()
            .filter(|checkout| matches(checkout, &self.query))
    }

    fn render_row(
        &self,
        checkout: &RepoCheckout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let repo = checkout.repo.clone();
        let place = match repo.host.as_str() {
            "github.com" => repo.owner.clone(),
            host => format!("{} · {host}", repo.owner),
        };
        let selected = self.selected.as_ref() == Some(&repo);
        let selector = format!("pr-repo-{}", repo.name);
        let id = repo.to_string();
        div()
            .id(id)
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
                    .flex_none()
                    .text_color(theme.foreground)
                    .child(repo.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(muted)
                    .child(place),
            )
            .when_some(checkout.branch.clone(), |row, branch| {
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
            .when(selected, |row| {
                row.child(
                    svg()
                        .flex_none()
                        .size(px(13.))
                        .path("icons/check.svg")
                        .text_color(theme.foreground),
                )
            })
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(RepoPicked(repo.clone()))))
    }
}

impl Render for RepoPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let shown: Vec<RepoCheckout> = self.shown().cloned().collect();
        // Until one is chosen this is the whole view, else a popover.
        let in_place = self.selected.is_none();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(in_place, |el| el.size_full().px_4().py_3())
            .when(!in_place, |el| el.w(px(320.)))
            .when(in_place, |el| {
                el.child(
                    div()
                        .pb_1()
                        .text_sm()
                        .text_color(muted)
                        .child("Which repository's pull requests?"),
                )
            })
            .child(Input::new(&self.search).with_size(Size::Small))
            .child(
                div()
                    .id("pr-repos")
                    .when(in_place, |el| el.flex_1().min_h_0())
                    .when(!in_place, |el| el.max_h(px(320.)))
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
                    .children(shown.iter().map(|checkout| self.render_row(checkout, cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str) -> RepoRef {
        RepoRef::new("github.com", "team", name)
    }

    fn checkout(name: &str) -> RepoCheckout {
        RepoCheckout {
            path: PathBuf::from("/folder").join(name),
            repo: repo(name),
            branch: None,
        }
    }

    fn found(checkouts: Vec<RepoCheckout>) -> SessionCheckouts {
        SessionCheckouts {
            directory: PathBuf::from("/folder"),
            checkouts,
        }
    }

    fn focus(found: &SessionCheckouts, last: Option<&str>, recent: &[&str]) -> Option<String> {
        let last = last.map(repo);
        let recent: Vec<RepoRef> = recent.iter().map(|name| repo(name)).collect();
        first_focus(found, last.as_ref(), &recent).map(|c| c.repo.name.clone())
    }

    #[test]
    fn a_folder_of_several_leaves_the_first_choice_open() {
        let several = found(vec![checkout("a"), checkout("b")]);
        assert_eq!(focus(&several, None, &[]), None);
        assert_eq!(focus(&several, None, &["gone"]), None);
    }

    #[test]
    fn what_was_listed_last_comes_back_then_the_folders_last_choice() {
        let several = found(vec![checkout("a"), checkout("b"), checkout("c")]);
        assert_eq!(focus(&several, Some("b"), &["c"]).as_deref(), Some("b"));
        assert_eq!(focus(&several, None, &["gone", "c"]).as_deref(), Some("c"));
        assert_eq!(focus(&several, Some("gone"), &[]), None);
    }

    #[test]
    fn a_folder_that_is_a_repository_itself_lists_that_one_first() {
        let root = RepoCheckout {
            path: PathBuf::from("/folder"),
            ..checkout("root")
        };
        let found = found(vec![root, checkout("nested")]);
        assert_eq!(focus(&found, None, &["nested"]).as_deref(), Some("root"));
        assert_eq!(
            focus(&found, Some("nested"), &[]).as_deref(),
            Some("nested")
        );
    }

    #[test]
    fn a_single_checkout_is_the_focus() {
        assert_eq!(
            focus(&found(vec![checkout("a")]), None, &[]).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn recent_ones_come_first_the_others_as_they_came() {
        let ordered = recent_first(
            vec![checkout("a"), checkout("b"), checkout("c"), checkout("d")],
            &[repo("c"), repo("gone"), repo("b")],
        );
        let names: Vec<_> = ordered.iter().map(|c| c.repo.name.as_str()).collect();
        assert_eq!(names, ["c", "b", "a", "d"]);
    }

    #[test]
    fn the_search_matches_names_owner_and_branch() {
        let tool = RepoCheckout {
            path: PathBuf::from("/folder/checked-out-as"),
            branch: Some("feature/x".into()),
            ..checkout("tool")
        };
        for query in ["", " TOOL ", "team/", "checked-out", "feature"] {
            assert!(matches(&tool, query), "{query}");
        }
        assert!(!matches(&tool, "other"));
    }
}
