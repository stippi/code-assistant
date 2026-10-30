//! The repository's pull requests: a search field, the Sort and Filters
//! menus and a refresh button above rows grouped by the viewer's part in
//! them (Authored, Review requested, Others).
//!
//! State and involvement (Filters) and the search's free text go to the
//! host: changing them asks for a new listing ([`PullRequestIntent::List`],
//! the text after a pause in typing). Sorting, the other filters and the
//! search's qualifiers (`label:bug`) work on the loaded rows
//! ([`list_logic`](super::list_logic)).

use super::PullRequestIntent;
use super::avatar::avatar;
use super::list_logic::{
    Arrangement, ChecksFilter, DraftFilter, Filters, GroupKey, ReviewFilter, SortMode, arrange,
    facets, parse_query,
};
use super::presentation::{
    GLYPH_SIZE, Tone, checks_glyph, conflict_glyph, diff_stat, hairline, relative_time,
    review_glyph,
};
use chrono::Utc;
use github::RepoRef;
use github::model::{Involvement, ListEntry, ListPage, ListQuery, ListState as ListedState};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Side, Sizable, Size};
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, ListAlignment, ListState, Pixels, Render,
    SharedString, Subscription, Task, WeakEntity, Window, div, list, prelude::*, px, svg,
};
use std::collections::HashMap;
use std::time::Duration;

/// Pull requests one listing asks for.
const PAGE_SIZE: u32 = 50;
/// Quiet typing time before the search text goes to the host.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);
const LIST_OVERDRAW: Pixels = px(400.);
/// Authors the Author filter offers at most.
const MAX_AUTHOR_CHOICES: usize = 15;

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Group {
        key: GroupKey,
        count: usize,
    },
    /// An index into the loaded entries.
    Entry(usize),
}

pub struct PullRequestList {
    repo: Option<RepoRef>,
    viewer: Option<String>,
    entries: Vec<ListEntry>,
    loaded: bool,
    /// The session's repository is still being looked for.
    resolving: bool,
    refreshing: bool,
    error: Option<String>,
    state: ListedState,
    involvement: Involvement,
    sort: SortMode,
    filters: Filters,
    search: Entity<InputState>,
    /// The search field's text.
    query: String,
    /// The free text of the last listing asked for.
    listed_text: String,
    rows: Vec<Row>,
    list: ListState,
    _search_debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for PullRequestList {}

impl PullRequestList {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search pull requests, or label:bug")
        });
        let subscriptions = vec![cx.subscribe_in(&search, window, Self::on_search_event)];
        Self {
            repo: None,
            viewer: None,
            entries: Vec::new(),
            loaded: false,
            resolving: false,
            refreshing: false,
            error: None,
            state: ListedState::default(),
            involvement: Involvement::default(),
            sort: SortMode::default(),
            filters: Filters::default(),
            search,
            query: String::new(),
            listed_text: String::new(),
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW),
            _search_debounce: None,
            _subscriptions: subscriptions,
        }
    }

    /// The repository listed; another one drops the loaded rows.
    pub fn set_repo(&mut self, repo: Option<RepoRef>, cx: &mut Context<Self>) {
        if self.repo == repo {
            return;
        }
        self.repo = repo;
        self.entries.clear();
        self.loaded = false;
        self.error = None;
        self.rebuild_rows();
        cx.notify();
    }

    /// The signed-in login, which decides the groups.
    pub fn set_viewer(&mut self, viewer: Option<String>, cx: &mut Context<Self>) {
        if self.viewer != viewer {
            self.viewer = viewer;
            self.rebuild_rows();
            cx.notify();
        }
    }

    pub fn set_page(&mut self, page: ListPage, cx: &mut Context<Self>) {
        self.entries = page.entries;
        self.loaded = true;
        self.error = None;
        self.rebuild_rows();
        cx.notify();
    }

    pub fn set_resolving(&mut self, resolving: bool, cx: &mut Context<Self>) {
        if self.resolving != resolving {
            self.resolving = resolving;
            cx.notify();
        }
    }

    /// A newer listing is being read; the held one stays shown.
    pub fn set_refreshing(&mut self, refreshing: bool, cx: &mut Context<Self>) {
        if self.refreshing != refreshing {
            self.refreshing = refreshing;
            cx.notify();
        }
    }

    pub fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    /// The listing the current state, involvement and search ask for.
    pub fn query(&self) -> ListQuery {
        let text = parse_query(&self.query).text;
        ListQuery {
            state: self.state,
            involvement: self.involvement,
            text: (!text.trim().is_empty()).then_some(text),
            limit: PAGE_SIZE,
            cursor: None,
        }
    }

    pub fn entry(&self, number: u64) -> Option<&ListEntry> {
        self.entries.iter().find(|e| e.number == number)
    }

    fn set_sort(&mut self, sort: SortMode, cx: &mut Context<Self>) {
        self.sort = sort;
        self.rebuild_rows();
        cx.notify();
    }

    fn set_filters(&mut self, change: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        change(&mut self.filters);
        self.rebuild_rows();
        cx.notify();
    }

    fn set_listing(
        &mut self,
        state: ListedState,
        involvement: Involvement,
        cx: &mut Context<Self>,
    ) {
        if (self.state, self.involvement) != (state, involvement) {
            self.state = state;
            self.involvement = involvement;
            cx.emit(PullRequestIntent::List(self.query()));
            cx.notify();
        }
    }

    fn on_search_event(
        &mut self,
        search: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        self.query = search.read(cx).value().to_string();
        self.rebuild_rows();
        cx.notify();
        let text = parse_query(&self.query).text;
        if text == self.listed_text {
            self._search_debounce = None;
            return;
        }
        self._search_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.listed_text = text;
                cx.emit(PullRequestIntent::List(this.query()));
            })
            .ok();
        }));
    }

    fn rebuild_rows(&mut self) {
        let repository = self
            .repo
            .as_ref()
            .map(RepoRef::full_name)
            .unwrap_or_default();
        let index: HashMap<u64, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.number, i))
            .collect();
        let groups = arrange(
            &self.entries,
            &Arrangement {
                sort: self.sort,
                query: &self.query,
                menu_filters: &self.filters,
                involvement: self.involvement,
                viewer: self.viewer.as_deref(),
                repository: &repository,
            },
        );
        self.rows = groups
            .iter()
            .flat_map(|group| {
                std::iter::once(Row::Group {
                    key: group.key,
                    count: group.entries.len(),
                })
                .chain(group.entries.iter().map(|e| Row::Entry(index[&e.number])))
            })
            .collect();
        self.list.reset(self.rows.len());
    }

    #[cfg(test)]
    fn shown(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| match row {
                Row::Group { key, count } => format!("{} {count}", key.label()),
                Row::Entry(i) => format!("#{}", self.entries[*i].number),
            })
            .collect()
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.rows.get(ix) {
            Some(Row::Group { key, count }) => render_group_header(*key, *count, cx),
            Some(Row::Entry(i)) => self.render_entry(*i, cx),
            None => div().into_any_element(),
        }
    }

    fn render_entry(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.entries[index];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let number = entry.number;
        let tone = Tone::of(entry.state, entry.is_draft);
        let conflict = conflict_glyph(
            entry.state,
            entry.is_draft,
            entry.mergeability,
            &entry.base_branch,
            cx,
        );
        let glyph = div()
            .relative()
            .flex_none()
            .mt(px(2.))
            .child(
                svg()
                    .size(px(16.))
                    .path(tone.icon())
                    .text_color(tone.color(cx)),
            )
            .when_some(conflict, |el, conflict| {
                el.child(
                    div()
                        .absolute()
                        .right(px(-4.))
                        .bottom(px(-4.))
                        .rounded_full()
                        .bg(cx.theme().background)
                        .child(conflict.render(px(10.))),
                )
            });
        let signals = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .children(
                entry
                    .checks_state
                    .map(|s| checks_glyph(s, cx).render(GLYPH_SIZE)),
            )
            .children(
                entry
                    .review_decision
                    .map(|d| review_glyph(d, cx).render(GLYPH_SIZE)),
            );
        let line_one = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .flex_none()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .text_color(muted)
                    .child(format!("#{number}")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(entry.title.clone()),
            )
            .child(signals)
            .child(div().flex_none().text_xs().children(diff_stat(
                entry.additions,
                entry.deletions,
                cx,
            )));
        let line_two = div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(muted)
            .child(avatar(entry.author.as_ref(), px(14.), cx))
            .child(
                div().min_w_0().truncate().child(
                    entry
                        .author
                        .as_ref()
                        .map_or("ghost".to_string(), |a| a.login.clone()),
                ),
            )
            .children(
                self.repo
                    .as_ref()
                    .map(|repo| div().min_w_0().truncate().child(repo.full_name())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .child(relative_time(entry.updated_at, Utc::now())),
            );
        div()
            .px_2()
            .py_0p5()
            .child(
                div()
                    .id(("pr-row", number))
                    .debug_selector(move || format!("pr-row-{number}"))
                    .px_2()
                    .py_1p5()
                    .flex()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.6)))
                    .child(glyph)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(line_one)
                            .child(line_two),
                    )
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(PullRequestIntent::Open(number))),
                    ),
            )
            .into_any_element()
    }

    /// Search field, then Sort, Filters and refresh.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let list = cx.entity().downgrade();
        let sort_list = list.clone();
        let line = hairline(cx);
        div()
            .flex_none()
            .px_4()
            .pt_3()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Input::new(&self.search)
                    .with_size(Size::Small)
                    .border_color(line)
                    .prefix(
                        Icon::default()
                            .path(SharedString::from("icons/magnifying_glass.svg"))
                            .with_size(Size::XSmall)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .cleanable(true),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Button::new("pr-sort")
                            .outline()
                            .small()
                            .border_color(line)
                            .icon(Icon::default().path("icons/arrow_up_down.svg"))
                            .label("Sort")
                            .dropdown_menu(move |menu, _, cx| sort_menu(menu, &sort_list, cx)),
                    )
                    .child(
                        Button::new("pr-filters")
                            .outline()
                            .small()
                            .border_color(line)
                            .icon(Icon::default().path("icons/list_filter.svg"))
                            .label("Filters")
                            .dropdown_menu(move |menu, window, cx| {
                                filters_menu(menu, &list, window, cx)
                            }),
                    )
                    .child(
                        div().debug_selector(|| "pr-list-refresh".into()).child(
                            Button::new("pr-list-refresh")
                                .outline()
                                .small()
                                .border_color(line)
                                .icon(Icon::default().path("icons/rotate_cw.svg"))
                                .loading(self.refreshing)
                                .tooltip("Refresh")
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(PullRequestIntent::Refresh)),
                                ),
                        ),
                    ),
            )
    }

    /// What shows instead of rows.
    fn render_note(&self, cx: &App) -> Option<AnyElement> {
        let text = if let Some(error) = &self.error {
            error.clone()
        } else if self.repo.is_none() && self.resolving {
            "Looking for the session's repository…".into()
        } else if self.repo.is_none() {
            "No GitHub repository for this session.".into()
        } else if !self.loaded {
            "Loading pull requests…".into()
        } else if self.rows.is_empty() {
            "No pull requests match.".into()
        } else {
            return None;
        };
        Some(
            div()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element(),
        )
    }
}

fn render_group_header(key: GroupKey, count: usize, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let icon = match key {
        GroupKey::Authored => "icons/pen_line.svg",
        GroupKey::ReviewRequested => "icons/eye.svg",
        GroupKey::Others => "icons/users.svg",
    };
    div()
        .px_4()
        .pt_2()
        .pb_1()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(muted)
        .child(svg().size(px(13.)).path(icon).text_color(muted))
        .child(key.label())
        .child(
            div()
                .text_color(muted.opacity(0.7))
                .child(count.to_string()),
        )
        .child(div().flex_1().h(px(1.)).ml_1().bg(hairline(cx)))
        .into_any_element()
}

fn icon(path: &'static str) -> Icon {
    Icon::default().path(SharedString::from(path))
}

fn sort_icon(sort: SortMode) -> &'static str {
    match sort {
        SortMode::MergeReadiness => "icons/list_checks.svg",
        SortMode::BlockedOnMe => "icons/user_lock.svg",
        SortMode::RecentlyUpdated => "icons/clock.svg",
        SortMode::Newest => "icons/calendar_arrow_down.svg",
        SortMode::Oldest => "icons/calendar_arrow_up.svg",
        SortMode::Largest => "icons/maximize_2.svg",
        SortMode::Smallest => "icons/minimize_2.svg",
    }
}

fn sort_menu(menu: PopupMenu, list: &WeakEntity<PullRequestList>, cx: &App) -> PopupMenu {
    let current = list.upgrade().map(|l| l.read(cx).sort);
    SortMode::ALL
        .into_iter()
        .fold(menu.check_side(Side::Right), |menu, sort| {
            let list = list.clone();
            menu.item(
                PopupMenuItem::new(sort.label())
                    .icon(icon(sort_icon(sort)))
                    .checked(current == Some(sort))
                    .on_click(move |_, _, cx| {
                        list.update(cx, |l, cx| l.set_sort(sort, cx)).ok();
                    }),
            )
        })
}

/// A submenu of choices, the current one checked.
fn choices<T: Copy + PartialEq + 'static>(
    menu: PopupMenu,
    list: &WeakEntity<PullRequestList>,
    current: T,
    options: &[(T, &'static str, &'static str)],
    pick: fn(&mut PullRequestList, T, &mut Context<PullRequestList>),
) -> PopupMenu {
    options.iter().fold(
        menu.check_side(Side::Right),
        |menu, &(value, label, path)| {
            let list = list.clone();
            menu.item(
                PopupMenuItem::new(label)
                    .icon(icon(path))
                    .checked(value == current)
                    .on_click(move |_, _, cx| {
                        list.update(cx, |l, cx| pick(l, value, cx)).ok();
                    }),
            )
        },
    )
}

const STATES: &[(ListedState, &str, &str)] = &[
    (ListedState::All, "All", "icons/layers.svg"),
    (ListedState::Open, "Open", "icons/git_pull_request.svg"),
    (
        ListedState::Closed,
        "Closed",
        "icons/git_pull_request_closed.svg",
    ),
    (ListedState::Merged, "Merged", "icons/git_merge.svg"),
];

const INVOLVEMENTS: &[(Involvement, &str, &str)] = &[
    (Involvement::All, "All", "icons/layers.svg"),
    (Involvement::ReviewRequested, "Reviewing", "icons/eye.svg"),
    (Involvement::Authored, "Authored", "icons/pen_line.svg"),
];

const DRAFTS: &[(Option<DraftFilter>, &str, &str)] = &[
    (None, "All", "icons/layers.svg"),
    (
        Some(DraftFilter::Only),
        "Drafts only",
        "icons/git_pull_request_draft.svg",
    ),
    (Some(DraftFilter::Hide), "Hide drafts", "icons/eye_off.svg"),
];

const REVIEWS: &[(Option<ReviewFilter>, &str, &str)] = &[
    (None, "All", "icons/layers.svg"),
    (
        Some(ReviewFilter::Approved),
        "Approved",
        "icons/circle_check.svg",
    ),
    (
        Some(ReviewFilter::ChangesRequested),
        "Changes requested",
        "icons/circle_x.svg",
    ),
    (
        Some(ReviewFilter::ReviewRequired),
        "Review required",
        "icons/circle_dashed.svg",
    ),
    (
        Some(ReviewFilter::None),
        "No reviews",
        "icons/circle_slash.svg",
    ),
];

const CHECKS: &[(Option<ChecksFilter>, &str, &str)] = &[
    (None, "All", "icons/layers.svg"),
    (
        Some(ChecksFilter::Passing),
        "Passing",
        "icons/circle_check.svg",
    ),
    (Some(ChecksFilter::Failing), "Failing", "icons/circle_x.svg"),
];

fn label_of<T: PartialEq>(options: &[(T, &'static str, &'static str)], value: &T) -> &'static str {
    options
        .iter()
        .find(|(v, _, _)| v == value)
        .map_or("", |(_, label, _)| label)
}

fn filters_menu(
    menu: PopupMenu,
    list: &WeakEntity<PullRequestList>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(this) = list.upgrade() else {
        return menu;
    };
    let this = this.read(cx);
    let (state, involvement) = (this.state, this.involvement);
    let filters = this.filters.clone();
    let viewer = this.viewer.clone();
    let facets = facets(&this.entries);

    let author_value = filters.author.clone().unwrap_or("Anyone".into());
    let labels_value = match filters.labels.first().map(Vec::len) {
        None | Some(0) => "Any".to_string(),
        Some(1) => filters.labels[0][0].clone(),
        Some(n) => format!("{n} labels"),
    };
    let submenu_label = |name: &str, value: &str| format!("{name}: {value}");

    let l = list.clone();
    let menu = menu.min_w(px(220.)).submenu_with_icon(
        Some(icon("icons/git_pull_request.svg")),
        submenu_label("State", label_of(STATES, &state)),
        window,
        cx,
        move |menu, _, _| {
            choices(menu, &l, state, STATES, |l, state, cx| {
                let involvement = l.involvement;
                l.set_listing(state, involvement, cx)
            })
        },
    );
    let l = list.clone();
    let menu = menu.submenu_with_icon(
        Some(icon("icons/layers.svg")),
        submenu_label("Involvement", label_of(INVOLVEMENTS, &involvement)),
        window,
        cx,
        move |menu, _, _| {
            choices(menu, &l, involvement, INVOLVEMENTS, |l, involvement, cx| {
                let state = l.state;
                l.set_listing(state, involvement, cx)
            })
        },
    );

    let l = list.clone();
    let current_author = filters.author.clone();
    let menu = menu.separator().submenu_with_icon(
        Some(icon("icons/user_round.svg")),
        submenu_label("Author", &author_value),
        window,
        cx,
        move |menu, _, _| {
            let mut logins: Vec<(Option<String>, String)> = vec![(None, "Anyone".into())];
            if viewer.is_some() {
                logins.push((Some("@me".into()), "Me".into()));
            }
            logins.extend(
                facets
                    .authors
                    .iter()
                    .take(MAX_AUTHOR_CHOICES)
                    .map(|(login, count)| (Some(login.clone()), format!("{login}  {count}"))),
            );
            logins
                .into_iter()
                .fold(menu.check_side(Side::Right), |menu, (author, label)| {
                    let l = l.clone();
                    let checked = author == current_author;
                    menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            let author = author.clone();
                            l.update(cx, |l, cx| l.set_filters(|f| f.author = author, cx))
                                .ok();
                        },
                    ))
                })
        },
    );

    let l = list.clone();
    let selected: Vec<String> = filters.labels.first().cloned().unwrap_or_default();
    let labels = facets.labels.clone();
    let menu = menu.submenu_with_icon(
        Some(icon("icons/tag.svg")),
        submenu_label("Labels", &labels_value),
        window,
        cx,
        move |menu, _, _| {
            let any = {
                let l = l.clone();
                PopupMenuItem::new("Any")
                    .checked(selected.is_empty())
                    .on_click(move |_, _, cx| {
                        l.update(cx, |l, cx| l.set_filters(|f| f.labels.clear(), cx))
                            .ok();
                    })
            };
            labels.iter().fold(
                menu.check_side(Side::Right).item(any),
                |menu, (label, count)| {
                    let l = l.clone();
                    let name = label.name.clone();
                    let checked = selected.iter().any(|s| s.eq_ignore_ascii_case(&name));
                    menu.item(
                        PopupMenuItem::new(format!("{name}  {count}"))
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                let name = name.clone();
                                l.update(cx, |l, cx| l.set_filters(|f| toggle_label(f, name), cx))
                                    .ok();
                            }),
                    )
                },
            )
        },
    );

    let (draft, review, checks) = (filters.draft, filters.review, filters.checks);
    let l = list.clone();
    let menu = menu.submenu_with_icon(
        Some(icon("icons/git_pull_request_draft.svg")),
        submenu_label("Draft", label_of(DRAFTS, &draft)),
        window,
        cx,
        move |menu, _, _| {
            choices(menu, &l, draft, DRAFTS, |l, draft, cx| {
                l.set_filters(|f| f.draft = draft, cx)
            })
        },
    );
    let l = list.clone();
    let menu = menu.submenu_with_icon(
        Some(icon("icons/user_check.svg")),
        submenu_label("Review", label_of(REVIEWS, &review)),
        window,
        cx,
        move |menu, _, _| {
            choices(menu, &l, review, REVIEWS, |l, review, cx| {
                l.set_filters(|f| f.review = review, cx)
            })
        },
    );
    let l = list.clone();
    menu.submenu_with_icon(
        Some(icon("icons/circle_check.svg")),
        submenu_label("Checks", label_of(CHECKS, &checks)),
        window,
        cx,
        move |menu, _, _| {
            choices(menu, &l, checks, CHECKS, |l, checks, cx| {
                l.set_filters(|f| f.checks = checks, cx)
            })
        },
    )
}

/// Labels picked in the menu form one group: any of them matches.
fn toggle_label(filters: &mut Filters, name: String) {
    let group = match filters.labels.first_mut() {
        Some(group) => group,
        None => {
            filters.labels.push(Vec::new());
            &mut filters.labels[0]
        }
    };
    match group
        .iter()
        .position(|held| held.eq_ignore_ascii_case(&name))
    {
        Some(at) => {
            group.remove(at);
        }
        None => group.push(name),
    }
    if group.is_empty() {
        filters.labels.clear();
    }
}

impl Render for PullRequestList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let note = self.render_note(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .children(note)
            .child(
                div().flex_1().min_h_0().child(
                    list(
                        self.list.clone(),
                        cx.processor(|this: &mut Self, ix: usize, _, cx| this.render_row(ix, cx)),
                    )
                    .size_full(),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{entry, entry_with, repo};
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn list_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<PullRequestList>,
        Rc<RefCell<Vec<PullRequestIntent>>>,
        &mut VisualTestContext,
    ) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| PullRequestList::new(window, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let intents = Rc::new(RefCell::new(Vec::new()));
        let sink = intents.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, intent: &PullRequestIntent, _| {
                sink.borrow_mut().push(intent.clone())
            })
            .detach()
        });
        view.update(cx, |view, cx| {
            view.set_repo(Some(repo()), cx);
            view.set_viewer(Some("me".into()), cx);
            view.set_page(
                ListPage {
                    entries: vec![
                        entry(281, "feat(sub-agents): one kind of sub-agent", "ann"),
                        entry_with(107, "feat(execute_command): gate RTK", "bob", |e| {
                            e.review_request_logins = vec!["me".into()]
                        }),
                        entry_with(286, "Custom file renderers", "me", |e| {
                            e.additions = 3488;
                            e.deletions = 146;
                        }),
                    ],
                    next_cursor: None,
                },
                cx,
            );
        });
        cx.run_until_parked();
        (view, intents, cx)
    }

    #[gpui_kit::test]
    fn rows_show_in_their_groups(cx: &mut TestAppContext) {
        let (view, _, cx) = list_view(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.shown(),
                [
                    "Authored 1",
                    "#286",
                    "Review requested 1",
                    "#107",
                    "Others 1",
                    "#281"
                ]
            )
        });
        assert!(cx.debug_bounds("pr-row-281").is_some(), "rows are drawn");
    }

    #[gpui_kit::test]
    fn a_click_on_a_row_opens_it(cx: &mut TestAppContext) {
        let (_, intents, cx) = list_view(cx);
        let bounds = cx.debug_bounds("pr-row-107").unwrap();
        cx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        assert_eq!(intents.borrow().as_slice(), [PullRequestIntent::Open(107)]);
    }

    #[gpui_kit::test]
    fn qualifiers_filter_at_once_and_text_lists_after_a_pause(cx: &mut TestAppContext) {
        let (view, intents, cx) = list_view(cx);
        view.update_in(cx, |view, window, cx| {
            view.search.update(cx, |s, cx| s.focus(window, cx))
        });
        cx.simulate_input("author:ann");
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.shown(), ["Others 1", "#281"]));
        assert!(intents.borrow().is_empty(), "a qualifier stays here");

        // Setting the value says nothing; typing does.
        view.update_in(cx, |view, window, cx| {
            view.search.update(cx, |s, cx| s.set_value("", window, cx))
        });
        cx.simulate_input("renderers");
        cx.run_until_parked();
        assert!(intents.borrow().is_empty(), "waits for a pause");
        cx.executor().advance_clock(SEARCH_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::List(ListQuery {
                text: Some("renderers".into()),
                limit: PAGE_SIZE,
                ..ListQuery::default()
            })]
        );
    }

    #[gpui_kit::test]
    fn another_state_asks_for_another_listing(cx: &mut TestAppContext) {
        let (view, intents, cx) = list_view(cx);
        view.update(cx, |view, cx| {
            view.set_listing(ListedState::Merged, Involvement::All, cx)
        });
        let query = view.read_with(cx, |view, _| view.query());
        assert_eq!(query.state, ListedState::Merged);
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::List(query)]
        );
    }

    #[test]
    fn menu_labels_form_one_group() {
        let mut filters = Filters::default();
        toggle_label(&mut filters, "bug".into());
        toggle_label(&mut filters, "ui".into());
        assert_eq!(
            filters.labels,
            vec![vec!["bug".to_string(), "ui".to_string()]]
        );
        toggle_label(&mut filters, "BUG".into());
        toggle_label(&mut filters, "ui".into());
        assert!(filters.labels.is_empty());
    }
}
