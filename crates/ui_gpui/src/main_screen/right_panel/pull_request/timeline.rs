//! The Timeline tab: the pull request's history on a rail of 32 px
//! markers (icons or avatars): opened, commits (a click asks for the Code
//! tab scoped to that commit, [`OpenCommit`]), plain comments folded into
//! groups ("3 comments · 2 authors"), review verdicts (muted once newer
//! commits came), review threads, merged or closed. Newest first unless
//! the tab row's toggle says otherwise ([`TimelineTab::set_newest_first`]).
//!
//! One virtualized list; cards are mounted when first drawn and kept per
//! comment or thread id across refreshes.

mod logic;

use super::PullRequestIntent;
use super::avatar::avatar;
use super::conversation::logic::{
    ConversationItem, ReviewOutcome, conversation_items, is_verdict_stale, newest_commit_at,
    splice_list,
};
use super::conversation::parts::open_on_github;
use super::conversation::{CardStyle, CommentCard, Location, ThreadCard};
use super::data::PullRequestData;
use super::detail::TabScroll;
use super::preferences;
use super::presentation::{Tone, diff_stat, relative_time};
use chrono::{DateTime, Utc};
use github::model::{Activity, Actor, Comment, Commit, Detail};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, Hsla, ListAlignment, ListState, Pixels,
    Render, SharedString, Subscription, Window, div, list, prelude::*, px, svg,
};
use logic::{TimelineEvent, TimelineRow, build_timeline, distinct_authors, timeline_rows};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const LIST_OVERDRAW: Pixels = px(600.);
const MARKER: Pixels = px(32.);
/// Left of the row text: the marker and a gap.
const RAIL_INDENT: Pixels = px(48.);

/// Show the Code tab scoped to this commit (its oid). The detail view
/// subscribes and switches tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCommit(pub String);

enum Card {
    Comment(Entity<CommentCard>),
    Thread(Entity<ThreadCard>),
}

struct MountedCard {
    card: Card,
    _intents: Subscription,
}

pub struct TimelineTab {
    data: Entity<PullRequestData>,
    /// What the rows were built from.
    detail: Option<Arc<Detail>>,
    activity: Option<Arc<Activity>>,
    newest_first: bool,
    /// The remembered order was taken over.
    order_read: bool,
    /// Newest first.
    events: Vec<TimelineEvent>,
    rows: Vec<TimelineRow>,
    keys: Vec<String>,
    list: ListState,
    newest_commit_at: Option<DateTime<Utc>>,
    /// Comment groups unfolded, by their first comment's id.
    open_groups: HashSet<String>,
    cards: HashMap<String, MountedCard>,
}

impl EventEmitter<PullRequestIntent> for TimelineTab {}
impl EventEmitter<OpenCommit> for TimelineTab {}

fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn row_key(row: &TimelineRow) -> String {
    match row {
        TimelineRow::Event(event) => match event.as_ref() {
            TimelineEvent::Opened { .. } => "opened".into(),
            TimelineEvent::Commit(commit) => format!("commit:{}", commit.oid),
            TimelineEvent::Comment(comment) => comment.id.clone(),
            TimelineEvent::Thread(thread) => thread.thread.id.clone(),
            TimelineEvent::Merged { .. } => "merged".into(),
            TimelineEvent::Closed { .. } => "closed".into(),
        },
        TimelineRow::Comments(comments) => {
            let ids: Vec<&str> = comments.iter().map(|c| c.id.as_str()).collect();
            format!("group:{}", ids.join(","))
        }
    }
}

impl TimelineTab {
    /// How far the tab is scrolled; the header condenses once it is.
    pub fn scroll(&self) -> TabScroll {
        TabScroll::of(&self.list)
    }

    pub fn new(data: Entity<PullRequestData>) -> Self {
        Self {
            data,
            detail: None,
            activity: None,
            // Taken from the preferences when first drawn.
            newest_first: true,
            order_read: false,
            events: Vec::new(),
            rows: Vec::new(),
            keys: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW),
            newest_commit_at: None,
            open_groups: HashSet::new(),
            cards: HashMap::new(),
        }
    }

    pub fn newest_first(&self) -> bool {
        self.newest_first
    }

    pub fn set_newest_first(&mut self, newest_first: bool, cx: &mut Context<Self>) {
        self.order_read = true;
        if self.newest_first != newest_first {
            self.newest_first = newest_first;
            preferences::update(cx, |p| p.conversation.timeline_oldest_first = !newest_first);
            self.rebuild_rows();
            cx.notify();
        }
    }

    pub fn toggle_order(&mut self, cx: &mut Context<Self>) {
        self.set_newest_first(!self.newest_first, cx);
    }

    /// Take over what the page knows now (each frame; cheap unless it
    /// changed).
    fn sync(&mut self, cx: &mut Context<Self>) {
        if !self.order_read {
            self.order_read = true;
            let newest_first = !preferences::read(cx).conversation.timeline_oldest_first;
            if newest_first != self.newest_first {
                self.newest_first = newest_first;
                self.rebuild_rows();
                // The tab row shows the order.
                cx.notify();
            }
        }
        let (detail, activity) = {
            let data = self.data.read(cx);
            (data.detail.clone(), data.activity.clone())
        };
        if same(&detail, &self.detail) && same(&activity, &self.activity) {
            return;
        }
        self.detail = detail;
        self.activity = activity;
        let (Some(detail), Some(activity)) = (self.detail.clone(), self.activity.clone()) else {
            return;
        };
        let conversation = conversation_items(&activity.comments, &activity.review_threads);
        self.newest_commit_at = newest_commit_at(&activity.commits);
        self.refresh_cards(&activity, &conversation, cx);
        self.events = build_timeline(&detail, &activity, &conversation);
        self.rebuild_rows();
    }

    /// Mounted cards take the new read; those of items gone go.
    fn refresh_cards(
        &mut self,
        activity: &Activity,
        conversation: &[ConversationItem],
        cx: &mut Context<Self>,
    ) {
        let comments: HashMap<&str, &Comment> = activity
            .comments
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();
        let threads: HashMap<&str, &ConversationItem> = conversation
            .iter()
            .filter(|item| item.thread().is_some())
            .map(|item| (item.id(), item))
            .collect();
        let newest = self.newest_commit_at;
        self.cards.retain(|id, mounted| match &mounted.card {
            Card::Comment(card) => match comments.get(id.as_str()) {
                Some(comment) => {
                    let stale = is_stale(comment, newest);
                    card.update(cx, |card, cx| {
                        card.set_comment((*comment).clone(), cx);
                        card.set_stale(stale, cx);
                    });
                    true
                }
                None => false,
            },
            Card::Thread(card) => match threads.get(id.as_str()).and_then(|i| i.thread()) {
                Some(item) => {
                    card.update(cx, |card, cx| card.set_item(item.clone(), cx));
                    true
                }
                None => false,
            },
        });
    }

    fn rebuild_rows(&mut self) {
        let rows = timeline_rows(self.events.clone(), self.newest_first);
        let keys: Vec<String> = rows.iter().map(row_key).collect();
        splice_list(&self.list, &self.keys, &keys);
        self.rows = rows;
        self.keys = keys;
    }

    fn toggle_group(&mut self, ix: usize, first: String, cx: &mut Context<Self>) {
        if !self.open_groups.remove(&first) {
            self.open_groups.insert(first);
        }
        self.list.remeasure_items(ix..ix + 1);
        cx.notify();
    }

    fn comment_card(&mut self, comment: &Comment, cx: &mut Context<Self>) -> Entity<CommentCard> {
        if let Some(MountedCard {
            card: Card::Comment(card),
            ..
        }) = self.cards.get(&comment.id)
        {
            return card.clone();
        }
        let data = self.data.clone();
        let stale = is_stale(comment, self.newest_commit_at);
        let location = comment.path.clone().map(|path| Location {
            path,
            line: None,
            outdated: false,
        });
        let comment = comment.clone();
        let id = comment.id.clone();
        let card = cx.new(|cx| {
            let mut card = CommentCard::new(data, comment, CardStyle::Entry, cx)
                .with_location(location)
                .with_agent_button(true)
                .with_github_link(true);
            card.set_stale(stale, cx);
            card
        });
        let intents = cx.subscribe(&card, |_, _, intent: &PullRequestIntent, cx| {
            cx.emit(intent.clone())
        });
        self.cards.insert(
            id,
            MountedCard {
                card: Card::Comment(card.clone()),
                _intents: intents,
            },
        );
        card
    }

    fn thread_card(
        &mut self,
        item: &super::conversation::logic::ThreadItem,
        cx: &mut Context<Self>,
    ) -> Entity<ThreadCard> {
        if let Some(MountedCard {
            card: Card::Thread(card),
            ..
        }) = self.cards.get(&item.thread.id)
        {
            return card.clone();
        }
        let data = self.data.clone();
        let item = item.clone();
        let id = item.thread.id.clone();
        let card = cx.new(|_| {
            ThreadCard::new(data, item)
                .show_location(true)
                .with_github_link(true)
        });
        let intents = cx.subscribe(&card, |_, _, intent: &PullRequestIntent, cx| {
            cx.emit(intent.clone())
        });
        self.cards.insert(
            id,
            MountedCard {
                card: Card::Thread(card.clone()),
                _intents: intents,
            },
        );
        card
    }

    #[cfg(test)]
    fn row_keys(&self) -> &[String] {
        &self.keys
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let first = ix == 0;
        let last = ix + 1 == self.rows.len();
        let (marker, content) = match row {
            TimelineRow::Comments(comments) => self.render_group(ix, &comments, cx),
            TimelineRow::Event(event) => self.render_event(*event, cx),
        };
        rail_row(first, last, marker, content, cx)
    }

    fn render_event(
        &mut self,
        event: TimelineEvent,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let muted = cx.theme().muted_foreground;
        match event {
            TimelineEvent::Opened { at, author } => (
                icon_marker(Tone::Open.icon(), muted, cx),
                lifecycle(author.as_ref(), "Pull request opened", at, cx),
            ),
            TimelineEvent::Merged { at } => (
                icon_marker(Tone::Merged.icon(), Tone::Merged.color(cx), cx),
                lifecycle(None, "Pull request merged", at, cx),
            ),
            TimelineEvent::Closed { at } => (
                icon_marker(Tone::Closed.icon(), Tone::Closed.color(cx), cx),
                lifecycle(None, "Pull request closed", at, cx),
            ),
            TimelineEvent::Commit(commit) => self.render_commit(&commit, cx),
            TimelineEvent::Comment(comment) => {
                let stale = is_stale(&comment, self.newest_commit_at);
                let marker = actor_marker(comment.author.as_ref(), stale, cx);
                let card = self.comment_card(&comment, cx);
                (marker, div().py_1().child(card).into_any_element())
            }
            TimelineEvent::Thread(item) => {
                let card = self.thread_card(&item, cx);
                let folded = !card.read(cx).is_expanded();
                let marker = actor_marker(item.root.author.as_ref(), folded, cx);
                (marker, card.into_any_element())
            }
        }
    }

    fn render_commit(&self, commit: &Commit, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let github_link = self.detail.as_ref().map(|detail| {
            let url = format!("{}/commits/{}", detail.url, commit.oid);
            open_on_github(format!("commit-{}-github", commit.oid).into(), url, cx)
        });
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let marker = match commit.authors.first() {
            Some(author) => actor_marker(Some(author), false, cx),
            None => icon_marker("icons/git_commit_horizontal.svg", muted, cx),
        };
        let oid = commit.oid.clone();
        let short: String = commit.oid.chars().take(7).collect();
        let headline = if commit.message_headline.is_empty() {
            "Untitled commit".to_owned()
        } else {
            commit.message_headline.clone()
        };
        let stat = match (commit.additions, commit.deletions) {
            (Some(additions), Some(deletions)) => diff_stat(additions, deletions, cx),
            _ => None,
        };
        let row = div()
            .id(SharedString::from(format!("commit-{}", commit.oid)))
            .flex_1()
            .min_w_0()
            .debug_selector({
                let selector = format!("pr-commit-{short}");
                move || selector.clone()
            })
            .min_h(MARKER)
            .flex()
            .items_center()
            .gap_2p5()
            .cursor_pointer()
            .group("commit")
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .group_hover("commit", |s| s.text_color(theme.primary))
                            .child(headline),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(short),
                            )
                            .child(relative_time(commit.committed_date, Utc::now())),
                    ),
            )
            .children(stat.map(|stat| div().flex_none().text_xs().child(stat)))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenCommit(oid.clone()))));
        let content = div()
            .flex()
            .items_center()
            .gap_1()
            .child(row)
            .children(github_link)
            .into_any_element();
        (marker, content)
    }

    fn render_group(
        &mut self,
        ix: usize,
        comments: &[Comment],
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let first = comments[0].id.clone();
        let open = self.open_groups.contains(&first);
        let authors = distinct_authors(comments);
        let marker = actor_marker(authors.first(), !open, cx);
        let cards: Vec<Entity<CommentCard>> = if open {
            comments.iter().map(|c| self.comment_card(c, cx)).collect()
        } else {
            Vec::new()
        };
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let count = comments.len();
        let header = div()
            .id(SharedString::from(format!("group-{first}")))
            .debug_selector({
                let selector = format!("pr-group-{first}");
                move || selector.clone()
            })
            .min_h(MARKER)
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(!open, |el| el.opacity(0.6))
            .hover(|s| s.opacity(1.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!(
                                "{count} {}",
                                if count == 1 { "comment" } else { "comments" }
                            )),
                    )
                    .child(div().truncate().text_xs().text_color(muted).child(format!(
                        "{} {} · {}",
                        authors.len(),
                        if authors.len() == 1 {
                            "author"
                        } else {
                            "authors"
                        },
                        relative_time(comments[0].created_at, Utc::now())
                    ))),
            )
            .child(
                svg()
                    .flex_none()
                    .size(px(13.))
                    .path(if open {
                        "icons/chevron_up.svg"
                    } else {
                        "icons/chevron_down.svg"
                    })
                    .text_color(muted),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(ix, first.clone(), cx)));
        let content = div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |el| {
                el.child(div().pt_1().flex().flex_col().gap_3().children(cards))
            })
            .into_any_element();
        (marker, content)
    }
}

fn is_stale(comment: &Comment, newest_commit_at: Option<DateTime<Utc>>) -> bool {
    ReviewOutcome::of(comment.review_state).is_some()
        && is_verdict_stale(comment.created_at, newest_commit_at)
}

/// One row of the rail: the line through the markers, the marker, then
/// the content.
fn rail_row(
    first: bool,
    last: bool,
    marker: AnyElement,
    content: AnyElement,
    cx: &Context<TimelineTab>,
) -> AnyElement {
    let line = cx.theme().border.opacity(0.6);
    let center = MARKER / 2.;
    div()
        .px_4()
        .pt(if first { px(16.) } else { px(0.) })
        .child(
            div()
                .relative()
                .pl(RAIL_INDENT)
                .pb_5()
                .min_h(MARKER)
                .when(!(first && last), |row| {
                    row.child(
                        div()
                            .absolute()
                            .left(center - px(0.5))
                            .w(px(1.))
                            .bg(line)
                            .top(if first { center } else { px(0.) })
                            .map(|el| if last { el.h(center) } else { el.bottom_0() }),
                    )
                })
                .child(div().absolute().left_0().top_0().child(marker))
                .child(content),
        )
        .into_any_element()
}

fn marker_box(cx: &Context<TimelineTab>) -> gpui_kit::Div {
    div()
        .size(MARKER)
        .flex()
        .items_center()
        .justify_center()
        .bg(cx.theme().background)
}

fn icon_marker(icon: &'static str, color: Hsla, cx: &Context<TimelineTab>) -> AnyElement {
    marker_box(cx)
        .child(svg().size(px(14.)).path(icon).text_color(color))
        .into_any_element()
}

/// The actor's avatar; `muted` for something folded or outdated.
fn actor_marker(actor: Option<&Actor>, muted: bool, cx: &Context<TimelineTab>) -> AnyElement {
    marker_box(cx)
        .child(
            div()
                .when(muted, |el| el.opacity(0.45))
                .child(avatar(actor, px(28.), cx)),
        )
        .into_any_element()
}

/// "ann Pull request opened", the time under it.
fn lifecycle(
    actor: Option<&Actor>,
    label: &'static str,
    at: DateTime<Utc>,
    cx: &Context<TimelineTab>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .min_h(MARKER)
        .flex()
        .flex_col()
        .justify_center()
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .when_some(actor, |row, actor| row.child(actor.login.clone()))
                .child(label),
        )
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child(relative_time(at, Utc::now())),
        )
        .into_any_element()
}

impl Render for TimelineTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);
        if self.rows.is_empty() {
            return div()
                .size_full()
                .px_4()
                .py_3()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Loading the timeline…")
                .into_any_element();
        }
        div()
            .size_full()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this: &mut Self, ix: usize, _, cx| this.render_row(ix, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::conversation::fixtures::{
        click, comment, commit, data_with_detail, review, window,
    };
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{actor, hours_ago};
    use github::model::ReviewState;
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn timeline(cx: &mut TestAppContext) -> (Entity<TimelineTab>, &mut VisualTestContext) {
        let data = data_with_detail(cx, Some("me"));
        data.update(cx, |data, cx| {
            data.activity = Some(Arc::new(Activity {
                author: Some(actor("daniel-kurzynski")),
                reviewers: Vec::new(),
                comments: vec![
                    comment("a", "ann", hours_ago(16)),
                    comment("b", "bob", hours_ago(15)),
                    review("v", "cid", ReviewState::Approved, hours_ago(14)),
                ],
                comment_count: 3,
                comments_truncated: false,
                review_threads: Vec::new(),
                review_threads_truncated: false,
                commits: vec![commit("d9aea40f", hours_ago(13))],
                reactions: Vec::new(),
            }));
            cx.notify();
        });
        window(cx, move |_| TimelineTab::new(data))
    }

    #[gpui_kit::test]
    fn the_rail_runs_newest_first_and_turns_around(cx: &mut TestAppContext) {
        let (tab, cx) = timeline(cx);
        tab.read_with(cx, |tab, _| {
            assert_eq!(
                tab.row_keys(),
                ["commit:d9aea40f", "v", "group:b,a", "opened"]
            )
        });
        tab.update(cx, |tab, cx| tab.set_newest_first(false, cx));
        tab.read_with(cx, |tab, _| {
            assert_eq!(
                tab.row_keys(),
                ["opened", "group:a,b", "v", "commit:d9aea40f"]
            )
        });
    }

    #[gpui_kit::test]
    fn a_commit_asks_for_the_code_tab(cx: &mut TestAppContext) {
        let (tab, cx) = timeline(cx);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        cx.update(|_, cx| {
            cx.subscribe(&tab, move |_, event: &OpenCommit, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach()
        });
        let row = cx.debug_bounds("pr-commit-d9aea40").expect("a commit row");
        cx.simulate_click(row.center(), gpui_kit::Modifiers::none());
        assert_eq!(opened.borrow().as_slice(), [OpenCommit("d9aea40f".into())]);
    }

    #[gpui_kit::test]
    fn events_open_on_github(cx: &mut TestAppContext) {
        let (tab, cx) = timeline(cx);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        cx.update(|_, cx| {
            cx.subscribe(&tab, move |_, intent: &PullRequestIntent, _| {
                sink.borrow_mut().push(intent.clone())
            })
            .detach()
        });
        click(cx, "commit-d9aea40f-github");
        click(cx, "c-v-github");
        assert_eq!(
            opened.borrow().as_slice(),
            [
                PullRequestIntent::OpenUrl(
                    "https://github.com/stippi/code-assistant/pull/290/commits/d9aea40f".into()
                ),
                PullRequestIntent::OpenUrl("https://github.com/c/v".into()),
            ]
        );
    }

    #[gpui_kit::test]
    fn the_remembered_order_applies(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut settings = crate::shared::settings::UiSettings::default();
            settings.pull_requests.conversation.timeline_oldest_first = true;
            cx.set_global(crate::UiSettingsGlobal(settings));
        });
        let (tab, cx) = timeline(cx);
        tab.read_with(cx, |tab, _| {
            assert!(!tab.newest_first());
            assert_eq!(tab.row_keys()[0], "opened");
        });
    }

    #[gpui_kit::test]
    fn a_group_unfolds_into_its_comments(cx: &mut TestAppContext) {
        let (tab, cx) = timeline(cx);
        tab.read_with(cx, |tab, _| {
            assert!(!tab.cards.contains_key("a"), "folded groups mount nothing")
        });
        let group = cx.debug_bounds("pr-group-b").expect("a group");
        cx.simulate_click(group.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        tab.read_with(cx, |tab, _| {
            assert!(tab.cards.contains_key("a") && tab.cards.contains_key("b"))
        });
    }
}
