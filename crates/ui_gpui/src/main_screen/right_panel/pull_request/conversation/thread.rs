//! A review thread: "○ Open · 3 comments" or "✓ Resolved" (a click folds
//! it), Outdated, Resolve / Unresolve, where it sits in the code (when
//! asked), its comments, "Load more comments" for the ones the conversation
//! read left out, and a reply box. A resolved thread starts folded to one
//! line. The Summary and Timeline tabs show threads with it, the Code tab
//! next to their lines.

use super::super::PullRequestIntent;
use super::super::data::PullRequestData;
use super::agent::{add_to_agent_button, thread_text};
use super::card::{CardStyle, CommentCard};
use super::editor::{EditorEvent, EditorOptions, MarkdownEditor};
use super::logic::{ThreadItem, merge_thread_comments, preview_text, thread_comment, thread_item};
use super::parts::{Collapsed, Location, location_line, open_on_github, render_collapsed};
use github::model::{Comment, ReviewThread, ThreadComment};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, Render, SharedString, Subscription, Window, div,
    prelude::*, px, svg,
};
use std::collections::HashMap;

struct MountedCard {
    card: Entity<CommentCard>,
    _intents: Subscription,
}

pub struct ThreadCard {
    data: Entity<PullRequestData>,
    item: ThreadItem,
    /// Comments read with "Load more comments", past the item's.
    loaded: Vec<ThreadComment>,
    /// The cursor after `loaded`; `None` before the first page.
    loaded_cursor: Option<Option<String>>,
    loading_more: bool,
    cards: HashMap<String, MountedCard>,
    expanded: bool,
    show_location: bool,
    resolving: bool,
    reply: Option<(Entity<MarkdownEditor>, Subscription)>,
    /// Shows ↗ to the thread on GitHub.
    github_link: bool,
}

impl EventEmitter<PullRequestIntent> for ThreadCard {}

impl ThreadCard {
    /// A thread of the conversation ([`conversation_items`](super::logic::conversation_items)).
    pub fn new(data: Entity<PullRequestData>, item: ThreadItem) -> Self {
        Self {
            expanded: !item.thread.is_resolved,
            data,
            item,
            loaded: Vec::new(),
            loaded_cursor: None,
            loading_more: false,
            cards: HashMap::new(),
            show_location: false,
            resolving: false,
            reply: None,
            github_link: false,
        }
    }

    /// A thread as the activity lists it (the Code tab). `None` for a
    /// thread without comments.
    pub fn for_thread(data: Entity<PullRequestData>, thread: &ReviewThread) -> Option<Self> {
        Some(Self::new(data, thread_item(thread, &HashMap::new())?))
    }

    /// Show ↗ to the thread on GitHub in the header.
    pub fn with_github_link(mut self, show: bool) -> Self {
        self.github_link = show;
        self
    }

    /// Show `path:line` under the header.
    pub fn show_location(mut self, show: bool) -> Self {
        self.show_location = show;
        self
    }

    #[cfg(test)]
    pub fn thread(&self) -> &ReviewThread {
        &self.item.thread
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// The conversation was read anew. Loaded pages and an open reply stay;
    /// a thread resolved or reopened meanwhile folds or unfolds.
    pub fn set_item(&mut self, item: ThreadItem, cx: &mut Context<Self>) {
        if item == self.item {
            return;
        }
        if item.thread.is_resolved != self.item.thread.is_resolved {
            self.expanded = !item.thread.is_resolved;
        }
        self.item = item;
        let comments = self.comments();
        for comment in comments {
            if let Some(mounted) = self.cards.get(&comment.id) {
                mounted
                    .card
                    .update(cx, |card, cx| card.set_comment(comment, cx));
            }
        }
        cx.notify();
    }

    /// [`Self::set_item`] for a thread as the activity lists it.
    pub fn set_thread(&mut self, thread: &ReviewThread, cx: &mut Context<Self>) {
        if let Some(item) = thread_item(thread, &HashMap::new()) {
            self.set_item(item, cx);
        }
    }

    pub fn set_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if self.expanded != expanded {
            self.expanded = expanded;
            cx.notify();
        }
    }

    /// Every comment read, oldest first: the conversation's, then pages
    /// loaded here.
    fn comments(&self) -> Vec<Comment> {
        let item = &self.item;
        let mut comments: Vec<Comment> = std::iter::once(item.root.clone())
            .chain(item.replies.iter().cloned())
            .collect();
        let more: Vec<Comment> = self
            .loaded
            .iter()
            .filter(|c| !comments.iter().any(|known| known.id == c.id))
            .map(|c| thread_comment(c, &item.thread.path))
            .collect();
        comments.extend(more);
        comments
    }

    /// The cursor for the next page, if there is one.
    fn next_cursor(&self) -> Option<String> {
        match &self.loaded_cursor {
            Some(cursor) => cursor.clone(),
            None => self.item.thread.next_comments_cursor.clone(),
        }
    }

    fn card(&mut self, comment: &Comment, cx: &mut Context<Self>) -> Entity<CommentCard> {
        if let Some(mounted) = self.cards.get(&comment.id) {
            return mounted.card.clone();
        }
        let data = self.data.clone();
        let comment = comment.clone();
        let id = comment.id.clone();
        let card = cx.new(|cx| CommentCard::new(data, comment, CardStyle::Entry, cx));
        let intents = cx.subscribe(&card, |_, _, intent: &PullRequestIntent, cx| {
            cx.emit(intent.clone())
        });
        self.cards.insert(
            id,
            MountedCard {
                card: card.clone(),
                _intents: intents,
            },
        );
        card
    }

    fn can(&self, cx: &gpui_kit::App) -> (bool, bool) {
        let data = self.data.read(cx);
        let permissions = data.detail.as_ref().map(|d| &d.viewer_permissions);
        (
            permissions.is_some_and(|p| p.comment),
            permissions.is_some_and(|p| p.resolve),
        )
    }

    pub fn toggle_resolved(&mut self, cx: &mut Context<Self>) {
        if self.resolving {
            return;
        }
        let resolved = !self.item.thread.is_resolved;
        self.resolving = true;
        cx.notify();
        let thread_id = self.item.thread.id.clone();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service
                .pull_request_resolve_thread(pr, thread_id, resolved)
                .await
        });
        cx.spawn(async move |this, cx| {
            let done = write.await.is_ok();
            this.update(cx, |this, cx| {
                this.resolving = false;
                if done {
                    // Shown until the conversation is read anew.
                    this.item.thread.is_resolved = resolved;
                    this.expanded = !resolved;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(cursor) = self.next_cursor() else {
            return;
        };
        if self.loading_more {
            return;
        }
        self.loading_more = true;
        cx.notify();
        let thread_id = self.item.thread.id.clone();
        let read = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service
                .pull_request_thread_comments(pr, thread_id, cursor)
                .await
        });
        cx.spawn(async move |this, cx| {
            let page = read.await;
            this.update(cx, |this, cx| {
                this.loading_more = false;
                if let Ok(page) = page {
                    this.loaded = merge_thread_comments(&this.loaded, &page.comments);
                    this.loaded_cursor = Some(page.next_cursor);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn start_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reply.is_some() {
            return;
        }
        let repo = self.data.read(cx).pr.repo.clone();
        let editor = cx.new(|cx| MarkdownEditor::new(repo, "", EditorOptions::REPLY, window, cx));
        let subscription = cx.subscribe(
            &editor,
            |this, editor, event: &EditorEvent, cx| match event {
                EditorEvent::Submit(body) => this.send_reply(editor, body.clone(), cx),
                EditorEvent::Cancel => {
                    this.reply = None;
                    cx.notify();
                }
            },
        );
        self.reply = Some((editor, subscription));
        cx.notify();
    }

    fn send_reply(&mut self, editor: Entity<MarkdownEditor>, body: String, cx: &mut Context<Self>) {
        editor.update(cx, |editor, cx| editor.set_busy(true, cx));
        let thread_id = self.item.thread.id.clone();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_reply(pr, thread_id, body).await
        });
        cx.spawn(async move |this, cx| {
            let sent = write.await.is_ok();
            this.update(cx, |this, cx| {
                if sent {
                    // The reply comes with the next read; a page read to
                    // the end may continue again.
                    this.reply = None;
                    if this.loaded_cursor == Some(None) {
                        this.loaded_cursor = Some(this.item.thread.next_comments_cursor.clone());
                    }
                } else {
                    editor.update(cx, |editor, cx| editor.set_busy(false, cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn location(&self) -> Location {
        Location {
            path: self.item.thread.path.clone(),
            line: self.item.thread.line,
            outdated: self.item.thread.is_outdated,
        }
    }

    fn render_folded(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let root = self.item.root.clone();
        let reactions = self
            .card(&root, cx)
            .update(cx, |card, cx| card.render_reactions(cx));
        let host = self.data.read(cx).pr.repo.host.clone();
        let location = self.show_location.then(|| self.location());
        let id = format!("t-{}", self.item.thread.id);
        let replies = self.item.thread.comment_count.saturating_sub(1);
        render_collapsed(
            Collapsed {
                id: &id,
                author: root.author.as_ref(),
                host: &host,
                created_at: root.created_at,
                url: root.url.as_deref(),
                replies,
                label: if self.item.thread.is_resolved {
                    "Resolved"
                } else {
                    "Open"
                },
                preview: preview_text(&root.body),
                location: location.as_ref(),
                reactions,
            },
            |this: &mut Self, cx| this.set_expanded(true, cx),
            cx,
        )
    }

    fn render_header(&self, can_resolve: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = format!("t-{}", self.item.thread.id);
        let pr = self.data.read(cx).pr.clone();
        let handed = thread_text(&pr, &self.item);
        let github_link = self
            .item
            .url
            .clone()
            .filter(|_| self.github_link)
            .map(|url| open_on_github(format!("{id}-github").into(), url, cx));
        let theme = cx.theme();
        let thread = &self.item.thread;
        let count = thread.comment_count.max(1 + self.item.replies.len() as u64);
        let (icon, color, state) = if thread.is_resolved {
            ("icons/circle_check.svg", theme.success, "Resolved")
        } else {
            ("icons/circle.svg", theme.muted_foreground, "Open")
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(svg().size(px(13.)).path(icon).text_color(color))
            .child(
                div()
                    .id(SharedString::from(format!("{id}-fold")))
                    .debug_selector({
                        let selector = format!("{id}-fold");
                        move || selector.clone()
                    })
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.foreground))
                    .child(format!(
                        "{state} · {count} {}",
                        if count == 1 { "comment" } else { "comments" }
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.set_expanded(false, cx))),
            )
            .when(thread.is_outdated, |row| row.child("Outdated"))
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector({
                        let selector = format!("{id}-agent");
                        move || selector.clone()
                    })
                    .child(add_to_agent_button(format!("{id}-agent").into(), handed)),
            )
            .children(github_link)
            .when(can_resolve, |row| {
                row.child(
                    div().debug_selector(|| format!("{id}-resolve")).child(
                        Button::new(SharedString::from(format!("{id}-resolve")))
                            .ghost()
                            .xsmall()
                            .label(if thread.is_resolved {
                                "Unresolve"
                            } else {
                                "Resolve"
                            })
                            .loading(self.resolving)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_resolved(cx))),
                    ),
                )
            })
    }
}

impl Render for ThreadCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.expanded {
            return self.render_folded(cx);
        }
        let (can_reply, can_resolve) = self.can(cx);
        let id = format!("t-{}", self.item.thread.id);
        let cards: Vec<Entity<CommentCard>> = self
            .comments()
            .iter()
            .map(|comment| self.card(comment, cx))
            .collect();
        let next_cursor = self.next_cursor();
        let unread = self.item.more_replies > 0 && next_cursor.is_none() && self.loaded.is_empty();
        let header = self.render_header(can_resolve, cx);
        let location = self
            .show_location
            .then(|| location_line(&self.location(), cx));
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let url = self.item.url.clone();
        div()
            .id(SharedString::from(id.clone()))
            .min_w_0()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border.opacity(0.7))
            .bg(theme.background)
            .child(header)
            .children(location)
            .child(div().flex().flex_col().gap_3().children(cards))
            .when_some(next_cursor, |el, _| {
                el.child(
                    div().child(
                        Button::new(SharedString::from(format!("{id}-more")))
                            .ghost()
                            .xsmall()
                            .label("Load more comments")
                            .loading(self.loading_more)
                            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                    ),
                )
            })
            .when(unread, |el| {
                let count = self.item.more_replies;
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(muted)
                        .child(format!(
                            "{count} more {}",
                            if count == 1 { "reply" } else { "replies" }
                        ))
                        .when_some(url, |row, url| {
                            row.child(
                                div()
                                    .id(SharedString::from(format!("{id}-on-github")))
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme.foreground).underline())
                                    .child("View on GitHub")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PullRequestIntent::OpenUrl(url.clone()))
                                    })),
                            )
                        }),
                )
            })
            .when(can_reply, |el| match &self.reply {
                Some((editor, _)) => el.child(editor.clone()),
                None => el.child(
                    div().debug_selector(|| format!("{id}-reply")).child(
                        Button::new(SharedString::from(format!("{id}-reply")))
                            .ghost()
                            .xsmall()
                            .label("Reply")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_reply(window, cx)),
                            ),
                    ),
                ),
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{agent_inbox, click, data_with_detail, thread, window};
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::hours_ago;
    use gpui_kit::TestAppContext;

    fn card(
        thread: ReviewThread,
        cx: &mut TestAppContext,
    ) -> (Entity<ThreadCard>, &mut gpui_kit::VisualTestContext) {
        let data = data_with_detail(cx, Some("me"));
        window(cx, move |_| {
            ThreadCard::for_thread(data, &thread)
                .unwrap()
                .show_location(true)
        })
    }

    #[gpui_kit::test]
    fn an_open_thread_shows_its_comments_and_a_reply_box(cx: &mut TestAppContext) {
        let (view, cx) = card(
            thread("t1", &[("a", hours_ago(3)), ("b", hours_ago(2))]),
            cx,
        );
        view.read_with(cx, |view, _| {
            assert!(view.is_expanded());
            assert_eq!(view.cards.len(), 2);
        });
        let reply = cx.debug_bounds("t-t1-reply").expect("a reply button");
        cx.simulate_click(reply.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.reply.is_some()));
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.reply.is_none()));
    }

    #[gpui_kit::test]
    fn a_resolved_thread_starts_folded_and_mounts_no_cards_but_the_root(cx: &mut TestAppContext) {
        let mut resolved = thread("t1", &[("a", hours_ago(3)), ("b", hours_ago(2))]);
        resolved.is_resolved = true;
        let (view, cx) = card(resolved, cx);
        view.read_with(cx, |view, cx| {
            assert!(!view.is_expanded());
            // Only the root's card, for its reactions; its body is not parsed.
            assert_eq!(view.cards.len(), 1);
            assert!(!view.cards["a"].card.read(cx).body().is_mounted());
        });
        let expand = cx.debug_bounds("t-t1-expand").expect("folded");
        cx.simulate_click(expand.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.is_expanded()));
    }

    #[gpui_kit::test]
    fn add_to_agent_hands_the_whole_thread_over(cx: &mut TestAppContext) {
        let (_, cx) = card(
            thread("t1", &[("a", hours_ago(3)), ("b", hours_ago(2))]),
            cx,
        );
        let inbox = agent_inbox(cx);
        click(cx, "t-t1-agent");
        let inbox = inbox.borrow();
        assert!(inbox[0].starts_with("Review thread on stippi/code-assistant#290, src/lib.rs:12"));
        assert!(inbox[0].contains("b in a thread"));
    }

    #[gpui_kit::test]
    fn a_failed_resolve_leaves_the_thread_as_it_was(cx: &mut TestAppContext) {
        let (view, cx) = card(thread("t1", &[("a", hours_ago(3))]), cx);
        let resolve = cx.debug_bounds("t-t1-resolve").expect("a resolve button");
        cx.simulate_click(resolve.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert!(!view.resolving);
            assert!(!view.thread().is_resolved);
            assert!(view.data.read(cx).error.is_some(), "the failure shows");
        });
    }

    #[gpui_kit::test]
    fn more_comments_load_from_the_threads_cursor(cx: &mut TestAppContext) {
        let mut long = thread("t1", &[("a", hours_ago(3))]);
        long.comment_count = 4;
        long.next_comments_cursor = Some("c1".into());
        let (view, cx) = card(long, cx);
        view.update(cx, |view, cx| view.load_more(cx));
        view.read_with(cx, |view, _| assert!(view.loading_more));
        cx.run_until_parked();
        // The read failed (no service): the cursor stays for another try.
        view.read_with(cx, |view, _| {
            assert!(!view.loading_more);
            assert_eq!(view.next_cursor().as_deref(), Some("c1"));
        });
    }
}
