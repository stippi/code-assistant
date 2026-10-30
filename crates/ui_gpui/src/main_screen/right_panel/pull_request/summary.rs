//! The Summary tab (GitHub's Conversation): reviewers and labels with
//! their pickers, the description (edited in place), the checks (folded),
//! and the comments: loose comments and review threads in one sequence,
//! bots' chatter and settled items hidden by default (a bot's verdict
//! never), newest first, ten conversations at a time.
//!
//! Everything is one virtualized list ([`rows`]); comment and thread cards
//! are mounted when their row is first drawn and kept per id, so a refresh
//! of the conversation keeps parsed bodies, open editors and pending
//! reactions.

mod candidates;
mod meta;
mod picker;
mod rows;

use super::PullRequestIntent;
use super::conversation::logic::{
    CommentFilter, ConversationItem, ReviewOutcome, ReviewerEntry, can_edit_description,
    comment_count, conversation_items, filter_items, is_verdict_stale, latest_review_outcomes,
    newest_commit_at, reviewer_entries, splice_list,
};
use super::conversation::{
    CardStyle, CommentBody, CommentCard, EditorEvent, EditorOptions, Location, MarkdownEditor,
    ThreadCard,
};
use super::data::PullRequestData;
use super::presentation::{GLYPH_SIZE, check_status_glyph};
use candidates::{OpenPicker, PickerKind};
use chrono::{DateTime, Utc};
use github::model::{Activity, Detail, PullRequestUpdate};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Side, Sizable};
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, ListAlignment, ListState, Pixels,
    Render, SharedString, Subscription, WeakEntity, Window, div, list, prelude::*, px, svg,
};
use meta::{label_chip, meta_label, none_label, reviewer_avatars};
use rows::{CommentsLayout, PAGE, Row, summary_rows};
use std::collections::HashMap;
use std::sync::Arc;

const LIST_OVERDRAW: Pixels = px(600.);

enum Card {
    Comment(Entity<CommentCard>),
    Thread(Entity<ThreadCard>),
}

struct MountedCard {
    card: Card,
    _intents: Subscription,
}

pub struct SummaryTab {
    data: Entity<PullRequestData>,
    /// What the rows were built from.
    detail: Option<Arc<Detail>>,
    activity: Option<Arc<Activity>>,
    items: Vec<ConversationItem>,
    index_of: HashMap<SharedString, usize>,
    reviewers: Vec<ReviewerEntry>,
    newest_commit_at: Option<DateTime<Utc>>,
    filter: CommentFilter,
    newest_first: bool,
    /// Conversations shown at most.
    count: usize,
    /// Comments the filter hides.
    hidden_comments: usize,
    description_open: bool,
    checks_open: bool,
    comments_open: bool,
    description: Option<CommentBody>,
    description_editor: Option<(Entity<MarkdownEditor>, Subscription)>,
    rows: Vec<Row>,
    list: ListState,
    cards: HashMap<SharedString, MountedCard>,
    reviewer_picker: Option<OpenPicker>,
    label_picker: Option<OpenPicker>,
    _data: Subscription,
}

impl EventEmitter<PullRequestIntent> for SummaryTab {}

fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl SummaryTab {
    pub fn new(data: Entity<PullRequestData>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&data, |this, _, cx| this.sync(cx));
        let mut this = Self {
            data,
            detail: None,
            activity: None,
            items: Vec::new(),
            index_of: HashMap::new(),
            reviewers: Vec::new(),
            newest_commit_at: None,
            filter: CommentFilter::default(),
            newest_first: true,
            count: PAGE,
            hidden_comments: 0,
            description_open: true,
            checks_open: false,
            comments_open: true,
            description: None,
            description_editor: None,
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW),
            cards: HashMap::new(),
            reviewer_picker: None,
            label_picker: None,
            _data: observe,
        };
        // The sections show before anything is read.
        this.rebuild_rows();
        this.sync(cx);
        this
    }

    /// Take over what the page knows now.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (detail, activity) = {
            let data = self.data.read(cx);
            (data.detail.clone(), data.activity.clone())
        };
        let detail_changed = !same(&detail, &self.detail);
        let activity_changed = !same(&activity, &self.activity);
        if detail_changed {
            self.detail = detail;
            self.sync_description(cx);
        }
        if activity_changed {
            self.activity = activity;
            self.sync_conversation(cx);
        }
        if detail_changed || activity_changed {
            let outcomes = self.activity.as_ref().map_or_else(Vec::new, |a| {
                latest_review_outcomes(&a.comments, &a.commits)
            });
            let requested = self.detail.as_ref().map_or(&[][..], |d| &d.reviewers[..]);
            self.reviewers = reviewer_entries(requested, &outcomes);
            self.rebuild_rows();
        }
        cx.notify();
    }

    fn sync_description(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let repo = self.data.read(cx).pr.repo.clone();
        match &mut self.description {
            Some(body) => body.set_text(&detail.body, &repo, cx),
            None => {
                self.description =
                    Some(CommentBody::new("pr-description", &detail.body, &repo).unclipped())
            }
        }
    }

    fn sync_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(activity) = self.activity.clone() else {
            return;
        };
        self.items = conversation_items(&activity.comments, &activity.review_threads);
        self.index_of = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| (SharedString::from(item.id().to_owned()), i))
            .collect();
        self.newest_commit_at = newest_commit_at(&activity.commits);
        // Cards of items gone go; the others take the new read.
        let index_of = &self.index_of;
        self.cards.retain(|id, _| index_of.contains_key(id));
        for (id, mounted) in &self.cards {
            let item = &self.items[self.index_of[id]];
            let stale = self.is_stale(item);
            match (&mounted.card, item) {
                (Card::Comment(card), ConversationItem::Comment(comment)) => {
                    card.update(cx, |card, cx| {
                        card.set_comment(comment.clone(), cx);
                        card.set_stale(stale, cx);
                    })
                }
                (Card::Thread(card), ConversationItem::Thread(thread)) => {
                    card.update(cx, |card, cx| card.set_item(thread.clone(), cx))
                }
                _ => {}
            }
        }
    }

    /// A verdict given before the newest commit.
    fn is_stale(&self, item: &ConversationItem) -> bool {
        let lead = item.lead();
        ReviewOutcome::of(lead.review_state).is_some()
            && is_verdict_stale(lead.created_at, self.newest_commit_at)
    }

    fn rebuild_rows(&mut self) {
        let shown = filter_items(&self.items, self.filter);
        self.hidden_comments =
            comment_count(&self.items) - comment_count(shown.iter().map(|&i| &self.items[i]));
        let ids: Vec<SharedString> = shown
            .iter()
            .map(|&i| SharedString::from(self.items[i].id().to_owned()))
            .collect();
        let truncated = self.activity.as_ref().is_some_and(|a| a.comments_truncated);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: self.newest_first,
            count: self.count,
            open: self.comments_open,
            note: self.activity.is_none() || ids.is_empty() || truncated,
        });
        splice_list(&self.list, &self.rows, &rows);
        self.rows = rows;
    }

    pub fn set_filter(&mut self, filter: CommentFilter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.rebuild_rows();
            cx.notify();
        }
    }

    pub fn toggle_order(&mut self, cx: &mut Context<Self>) {
        self.newest_first = !self.newest_first;
        self.rebuild_rows();
        cx.notify();
    }

    fn show_more(&mut self, more: bool, cx: &mut Context<Self>) {
        self.count = if more { self.count + PAGE } else { PAGE };
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle_section(&mut self, row: Row, cx: &mut Context<Self>) {
        match row {
            Row::Description => self.description_open = !self.description_open,
            Row::Checks => self.checks_open = !self.checks_open,
            Row::CommentsHeader => {
                self.comments_open = !self.comments_open;
                self.rebuild_rows();
            }
            _ => return,
        }
        // The section's own height changed.
        if let Some(ix) = self.rows.iter().position(|r| *r == row) {
            self.list.remeasure_items(ix..ix + 1);
        }
        cx.notify();
    }

    #[cfg(test)]
    fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn card(&mut self, id: &SharedString, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(mounted) = self.cards.get(id) {
            return Some(match &mounted.card {
                Card::Comment(card) => card.clone().into_any_element(),
                Card::Thread(card) => card.clone().into_any_element(),
            });
        }
        let item = self.items.get(*self.index_of.get(id)?)?.clone();
        let stale = self.is_stale(&item);
        let data = self.data.clone();
        let forward = |_: &mut Self, intent: &PullRequestIntent, cx: &mut Context<Self>| {
            cx.emit(intent.clone())
        };
        let (card, intents, element) = match item {
            ConversationItem::Comment(comment) => {
                let location = comment.path.clone().map(|path| Location {
                    path,
                    line: None,
                    outdated: false,
                });
                let dismissed = item_is_dismissed(&comment.review_state);
                let card = cx.new(|cx| {
                    let mut card = CommentCard::new(data, comment, CardStyle::Card, cx)
                        .with_location(location)
                        .collapsed(dismissed);
                    card.set_stale(stale, cx);
                    card
                });
                let intents =
                    cx.subscribe(&card, move |this, _, intent, cx| forward(this, intent, cx));
                let element = card.clone().into_any_element();
                (Card::Comment(card), intents, element)
            }
            ConversationItem::Thread(thread) => {
                let card = cx.new(|_| ThreadCard::new(data, thread).show_location(true));
                let intents =
                    cx.subscribe(&card, move |this, _, intent, cx| forward(this, intent, cx));
                let element = card.clone().into_any_element();
                (Card::Thread(card), intents, element)
            }
        };
        self.cards.insert(
            id.clone(),
            MountedCard {
                card,
                _intents: intents,
            },
        );
        Some(element)
    }

    fn edit_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let repo = self.data.read(cx).pr.repo.clone();
        let editor = cx.new(|cx| {
            MarkdownEditor::new(repo, &detail.body, EditorOptions::DESCRIPTION, window, cx)
        });
        let subscription = cx.subscribe(
            &editor,
            |this, editor, event: &EditorEvent, cx| match event {
                EditorEvent::Submit(body) => this.save_description(editor, body.clone(), cx),
                EditorEvent::Cancel => this.close_description_editor(cx),
            },
        );
        self.description_editor = Some((editor, subscription));
        self.remeasure(Row::Description);
        cx.notify();
    }

    fn close_description_editor(&mut self, cx: &mut Context<Self>) {
        self.description_editor = None;
        self.remeasure(Row::Description);
        cx.notify();
    }

    fn save_description(
        &mut self,
        editor: Entity<MarkdownEditor>,
        body: String,
        cx: &mut Context<Self>,
    ) {
        editor.update(cx, |editor, cx| editor.set_busy(true, cx));
        let sent = body.clone();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            let update = PullRequestUpdate {
                body: Some(sent),
                ..PullRequestUpdate::default()
            };
            service.pull_request_update(pr, update).await
        });
        cx.spawn(async move |this, cx| {
            let saved = write.await.is_ok();
            this.update(cx, |this, cx| {
                if saved {
                    // Shown until the detail is read anew.
                    let repo = this.data.read(cx).pr.repo.clone();
                    if let Some(description) = &mut this.description {
                        description.set_text(&body, &repo, cx);
                    }
                    this.close_description_editor(cx);
                } else {
                    editor.update(cx, |editor, cx| editor.set_busy(false, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn remeasure(&self, row: Row) {
        if let Some(ix) = self.rows.iter().position(|r| *r == row) {
            self.list.remeasure_items(ix..ix + 1);
        }
    }

    fn render_meta(&self, cx: &mut Context<Self>) -> AnyElement {
        let permissions = self.detail.as_ref().map(|d| &d.viewer_permissions);
        let can_request = permissions.is_some_and(|p| p.request_reviewers);
        let can_label = permissions.is_some_and(|p| p.labels);
        let labels = self.detail.as_ref().map_or(&[][..], |d| &d.labels[..]);
        let reviewers = if self.reviewers.is_empty() {
            none_label(cx)
        } else {
            reviewer_avatars(&self.reviewers, cx)
        };
        let label_chips: Vec<AnyElement> = if labels.is_empty() {
            vec![none_label(cx)]
        } else {
            labels.iter().map(|label| label_chip(label, cx)).collect()
        };
        let row = || div().min_h(px(28.)).flex().items_center().gap_2();
        div()
            .px_4()
            .pt_3()
            .pb_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                row()
                    .child(meta_label("icons/users.svg", "Reviewers", cx))
                    .child(reviewers)
                    .when(can_request, |row| {
                        row.child(self.picker_trigger(PickerKind::Reviewers, cx))
                    }),
            )
            .when(!labels.is_empty() || can_label, |el| {
                el.child(
                    row()
                        .child(meta_label("icons/tag.svg", "Labels", cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .children(label_chips)
                                .when(can_label, |row| {
                                    row.child(self.picker_trigger(PickerKind::Labels, cx))
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    /// "Description ▾" and the like; a click folds the section.
    fn section_header(
        &self,
        row: Row,
        title: SharedString,
        open: bool,
        actions: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let id = SharedString::from(format!("pr-section-{title}"));
        div()
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .hover(|s| s.text_color(theme.foreground))
                    .child(title)
                    .child(
                        svg()
                            .size(px(13.))
                            .path(if open {
                                "icons/chevron_down.svg"
                            } else {
                                "icons/chevron_right.svg"
                            })
                            .text_color(muted.opacity(0.6)),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_section(row.clone(), cx)),
                    ),
            )
            .children(actions)
    }

    fn render_description(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let viewer = self.data.read(cx).viewer.as_ref().map(|v| v.login.clone());
        let editable = self.description_editor.is_none()
            && self
                .detail
                .as_ref()
                .is_some_and(|d| can_edit_description(d, viewer.as_deref()));
        let muted = cx.theme().muted_foreground;
        let pencil = editable.then(|| {
            div()
                .debug_selector(|| "pr-description-edit".into())
                .child(
                    Button::new("pr-description-edit")
                        .ghost()
                        .xsmall()
                        .icon(Icon::default().path("icons/pencil.svg").text_color(muted))
                        .tooltip("Edit description")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.edit_description(window, cx)),
                        ),
                )
                .into_any_element()
        });
        let header = self.section_header(
            Row::Description,
            "Description".into(),
            self.description_open,
            pencil,
            cx,
        );
        let body = if !self.description_open {
            None
        } else if let Some((editor, _)) = &self.description_editor {
            Some(editor.clone().into_any_element())
        } else {
            let background = cx.theme().background;
            match &mut self.description {
                Some(body) if !body.is_empty() => Some(body.render(
                    background,
                    |this: &mut Self| this.description.as_mut().expect("rendered"),
                    cx,
                )),
                Some(_) => Some(
                    div()
                        .text_sm()
                        .italic()
                        .text_color(muted)
                        .child("No description provided.")
                        .into_any_element(),
                ),
                None => None,
            }
        };
        div()
            .child(header)
            .children(body.map(|body| div().px_4().pb_4().child(body)))
            .into_any_element()
    }

    fn render_checks(&self, cx: &mut Context<Self>) -> AnyElement {
        let header = self.section_header(Row::Checks, "Checks".into(), self.checks_open, None, cx);
        let checks = self.data.read(cx).current_checks().to_vec();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let body = self.checks_open.then(|| {
            if checks.is_empty() {
                return div()
                    .text_xs()
                    .text_color(muted)
                    .child("No checks reported.")
                    .into_any_element();
            }
            div()
                .flex()
                .flex_col()
                .children(checks.iter().enumerate().map(|(i, check)| {
                    let glyph = check_status_glyph(check, cx);
                    let url = check.url.clone();
                    div()
                        .id(("pr-summary-check", i))
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(glyph.render(GLYPH_SIZE))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(check.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(glyph.label.clone()),
                        )
                        .when_some(url, |row, url| {
                            row.cursor_pointer()
                                .hover(|s| s.bg(theme.muted))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(PullRequestIntent::OpenUrl(url.clone()))
                                }))
                        })
                }))
                .into_any_element()
        });
        div()
            .child(header)
            .children(body.map(|body| div().px_4().pb_4().child(body)))
            .into_any_element()
    }

    fn render_comments_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.activity.as_ref().map_or(0, |a| a.comment_count);
        let this = cx.entity().downgrade();
        let hidden = self.hidden_comments;
        let actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("pr-comments-filter")
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path("icons/list_filter.svg"))
                    .label(if hidden > 0 {
                        SharedString::from(format!("{hidden} hidden"))
                    } else {
                        "Filter".into()
                    })
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, cx| {
                        filter_menu(menu, &this, cx)
                    }),
            )
            .child(
                div().debug_selector(|| "pr-comments-order".into()).child(
                    Button::new("pr-comments-order")
                        .ghost()
                        .xsmall()
                        .icon(Icon::default().path("icons/arrow_up_down.svg"))
                        .label(if self.newest_first {
                            "Newest first"
                        } else {
                            "Oldest first"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_order(cx))),
                ),
            )
            .into_any_element();
        self.section_header(
            Row::CommentsHeader,
            format!("Comments ({count})").into(),
            self.comments_open,
            Some(actions),
            cx,
        )
        .into_any_element()
    }

    fn render_comments_note(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let note = |text: String| div().py_2().text_xs().text_color(muted).child(text);
        let Some(activity) = &self.activity else {
            return div()
                .px_4()
                .child(note("Loading the conversation…".into()))
                .into_any_element();
        };
        let read = comment_count(&self.items);
        let shown = read - self.hidden_comments;
        div()
            .px_4()
            .pb_2()
            .flex()
            .flex_col()
            .gap_1()
            .when(activity.comments_truncated, |el| {
                el.child(
                    div()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.warning.opacity(0.3))
                        .bg(theme.warning.opacity(0.08))
                        .text_xs()
                        .child(format!(
                            "This conversation is only partly read: {read} {} shown here. \
                             Open it on GitHub to read the rest.",
                            if read == 1 {
                                "comment is"
                            } else {
                                "comments are"
                            }
                        )),
                )
            })
            .when(self.items.is_empty(), |el| {
                el.child(note("No comments yet.".into()))
            })
            .when(!self.items.is_empty() && shown == 0, |el| {
                el.child(note(if self.hidden_comments == 1 {
                    "The only comment is hidden by the filter.".into()
                } else {
                    format!(
                        "All {} comments are hidden by the filter.",
                        self.hidden_comments
                    )
                }))
            })
            .into_any_element()
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        match row {
            Row::Meta => self.render_meta(cx),
            Row::Description => self.render_description(cx),
            Row::Checks => self.render_checks(cx),
            Row::CommentsHeader => self.render_comments_header(cx),
            Row::CommentsNote => self.render_comments_note(cx),
            Row::ShowOlder { hidden } => {
                let more = hidden.min(PAGE);
                div()
                    .px_4()
                    .pb_3()
                    .child(
                        Button::new("pr-comments-older")
                            .outline()
                            .small()
                            .w_full()
                            .label(format!(
                                "Show {more} older {} ({hidden} hidden)",
                                if more == 1 {
                                    "conversation"
                                } else {
                                    "conversations"
                                }
                            ))
                            .on_click(cx.listener(|this, _, _, cx| this.show_more(true, cx))),
                    )
                    .into_any_element()
            }
            Row::ShowRecent => div()
                .px_4()
                .pb_3()
                .child(
                    Button::new("pr-comments-recent")
                        .ghost()
                        .small()
                        .w_full()
                        .label(format!("Show only the {PAGE} most recent conversations"))
                        .on_click(cx.listener(|this, _, _, cx| this.show_more(false, cx))),
                )
                .into_any_element(),
            Row::Item(id) => match self.card(&id, cx) {
                Some(card) => div().px_4().pb_3().child(card).into_any_element(),
                None => div().into_any_element(),
            },
        }
    }
}

fn item_is_dismissed(state: &Option<github::model::ReviewState>) -> bool {
    ReviewOutcome::of(*state) == Some(ReviewOutcome::Dismissed)
}

/// Show: bot comments, resolved or dismissed.
fn filter_menu(menu: PopupMenu, tab: &WeakEntity<SummaryTab>, cx: &App) -> PopupMenu {
    let Some(filter) = tab.upgrade().map(|t| t.read(cx).filter) else {
        return menu;
    };
    let option = |label: &'static str, checked: bool, change: fn(&mut CommentFilter)| {
        let tab = tab.clone();
        PopupMenuItem::new(label)
            .checked(checked)
            .on_click(move |_, _, cx| {
                tab.update(cx, |tab, cx| {
                    let mut filter = tab.filter;
                    change(&mut filter);
                    tab.set_filter(filter, cx);
                })
                .ok();
            })
    };
    menu.check_side(Side::Right)
        .label("Show")
        .item(option("Bot comments", filter.bots, |f| f.bots = !f.bots))
        .item(option("Resolved or dismissed", filter.resolved, |f| {
            f.resolved = !f.resolved
        }))
}

impl Render for SummaryTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            list(
                self.list.clone(),
                cx.processor(|this: &mut Self, ix: usize, _, cx| this.render_row(ix, cx)),
            )
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::conversation::fixtures::{
        bot, comment, data_with_detail, review, thread, window,
    };
    use super::picker::{Candidate, Leading, PickerEntry, PickerToggle};
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{actor, detail_with, hours_ago};
    use github::model::ReviewState;
    use gpui_kit::{TestAppContext, VisualTestContext};

    fn activity(n: usize) -> Activity {
        let mut comments: Vec<_> = (0..n)
            .map(|i| comment(&format!("c{i}"), "ann", hours_ago(100 - i as i64)))
            .collect();
        comments.push(review("ok", "bob", ReviewState::Approved, hours_ago(2)));
        comments.push({
            let mut chatter = comment("bot", "x", hours_ago(1));
            chatter.author = Some(bot("ci-bot"));
            chatter
        });
        let mut resolved = thread("t-done", &[("r1", hours_ago(50))]);
        resolved.is_resolved = true;
        Activity {
            author: Some(actor("ann")),
            reviewers: Vec::new(),
            comment_count: comments.len() as u64 + 1,
            comments,
            comments_truncated: false,
            review_threads: vec![resolved],
            review_threads_truncated: false,
            commits: Vec::new(),
            reactions: Vec::new(),
        }
    }

    fn summary(
        activity: Activity,
        cx: &mut TestAppContext,
    ) -> (Entity<SummaryTab>, &mut VisualTestContext) {
        let data = data_with_detail(cx, Some("ann"));
        data.update(cx, |data, cx| {
            data.activity = Some(Arc::new(activity));
            cx.notify();
        });
        window(cx, move |cx| SummaryTab::new(data, cx))
    }

    fn items(tab: &SummaryTab) -> Vec<String> {
        tab.rows()
            .iter()
            .filter_map(|row| match row {
                Row::Item(id) => Some(id.to_string()),
                Row::ShowOlder { hidden } => Some(format!("older {hidden}")),
                _ => None,
            })
            .collect()
    }

    #[gpui_kit::test]
    fn the_newest_conversations_show_without_bots_and_settled_threads(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(12), cx);
        tab.read_with(cx, |tab, _| {
            let items = items(tab);
            // The approval (a verdict) shows; the bot's chatter and the
            // resolved thread do not.
            assert_eq!(items[0], "ok");
            assert!(!items.contains(&"bot".to_string()));
            assert!(!items.contains(&"t-done".to_string()));
            assert_eq!(items.len(), PAGE + 1);
            assert_eq!(items.last().unwrap(), "older 3");
            assert_eq!(tab.hidden_comments, 2);
        });
    }

    #[gpui_kit::test]
    fn the_filter_and_the_order_rebuild_the_rows(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(2), cx);
        tab.update(cx, |tab, cx| {
            tab.set_filter(
                CommentFilter {
                    bots: true,
                    resolved: true,
                },
                cx,
            )
        });
        tab.read_with(cx, |tab, _| {
            assert_eq!(items(tab), ["bot", "ok", "t-done", "c1", "c0"]);
        });
        let order = cx
            .debug_bounds("pr-comments-order")
            .expect("an order toggle");
        cx.simulate_click(order.center(), gpui_kit::Modifiers::none());
        tab.read_with(cx, |tab, _| {
            assert_eq!(items(tab), ["c0", "c1", "t-done", "ok", "bot"]);
        });
    }

    #[gpui_kit::test]
    fn cards_mount_when_drawn_and_survive_a_refresh(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(2), cx);
        let card = tab.read_with(cx, |tab, _| {
            match &tab.cards.get("c1").expect("drawn").card {
                Card::Comment(card) => card.clone(),
                Card::Thread(_) => panic!("a comment"),
            }
        });
        let data = tab.read_with(cx, |tab, _| tab.data.clone());
        let mut refreshed = activity(2);
        refreshed.comments[1].body = "edited".into();
        data.update(cx, |data, cx| {
            data.activity = Some(Arc::new(refreshed));
            cx.notify();
        });
        cx.run_until_parked();
        tab.read_with(cx, |tab, cx| {
            let Card::Comment(same) = &tab.cards["c1"].card else {
                panic!("a comment")
            };
            assert_eq!(same.entity_id(), card.entity_id());
            assert_eq!(same.read(cx).comment().body, "edited");
        });
    }

    #[gpui_kit::test]
    fn the_author_edits_the_description_in_place(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(0), cx);
        // ann is not the author; the fixture's viewer may merge.
        let edit = cx.debug_bounds("pr-description-edit").expect("a pencil");
        cx.simulate_click(edit.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        tab.read_with(cx, |tab, _| assert!(tab.description_editor.is_some()));
        cx.simulate_keystrokes("escape");
        tab.read_with(cx, |tab, _| assert!(tab.description_editor.is_none()));
    }

    #[gpui_kit::test]
    fn a_picker_reads_candidates_and_ticks_only_what_was_written(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(0), cx);
        tab.update_in(cx, |tab, window, cx| {
            tab.open_picker(PickerKind::Labels, window, cx)
        });
        cx.run_until_parked();
        let picker = tab.read_with(cx, |tab, _| {
            tab.label_picker.as_ref().expect("open").picker.clone()
        });
        let bug = Candidate::Label("bug".into());
        picker.update(cx, |picker, cx| {
            // No session service: the read failed.
            assert!(picker.entries().is_empty());
            picker.set_entries(
                vec![PickerEntry {
                    candidate: bug.clone(),
                    label: "bug".into(),
                    detail: None,
                    leading: Leading::Dot(None),
                    checked: false,
                }],
                false,
                cx,
            );
            cx.emit(PickerToggle(bug, true));
        });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert!(!picker.entries()[0].checked, "the write failed")
        });
        tab.update(cx, |tab, cx| tab.close_picker(PickerKind::Labels, cx));
        tab.read_with(cx, |tab, _| assert!(tab.label_picker.is_none()));
    }

    #[gpui_kit::test]
    fn a_pull_request_opens_at_the_top(cx: &mut TestAppContext) {
        // Nothing read yet when the tab first draws.
        let data = super::super::conversation::fixtures::data(cx, Some("ann"));
        let shown = data.clone();
        let (tab, cx) = window(cx, move |cx| SummaryTab::new(shown, cx));
        // The detail, then the conversation arrive while the tab shows.
        let long = (0..80)
            .map(|i| format!("Paragraph {i} of a long description."))
            .collect::<Vec<_>>()
            .join("\n\n");
        data.update(cx, |data, cx| {
            data.detail = Some(Arc::new(detail_with(290, |d| d.body = long)));
            cx.notify();
        });
        cx.run_until_parked();
        data.update(cx, |data, cx| {
            data.activity = Some(Arc::new(activity(3)));
            cx.notify();
        });
        cx.run_until_parked();
        tab.read_with(cx, |tab, _| {
            let top = tab.list.logical_scroll_top();
            assert_eq!((top.item_ix, top.offset_in_item), (0, px(0.)));
        });
    }

    #[gpui_kit::test]
    fn folding_the_comments_drops_their_rows(cx: &mut TestAppContext) {
        let (tab, cx) = summary(activity(2), cx);
        let header = cx
            .debug_bounds("pr-section-Comments (5)")
            .expect("the comments header");
        cx.simulate_click(header.center(), gpui_kit::Modifiers::none());
        tab.read_with(cx, |tab, _| assert!(items(tab).is_empty()));
    }
}
