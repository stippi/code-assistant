//! One comment: who wrote it and when (links to the profile and the
//! comment), a review's verdict, the reactions, an edit pencil for the
//! viewer's own comments (the [`MarkdownEditor`] in place, saved through
//! `pull_request_update_comment`), where it sits in the code, and the body.
//!
//! A card keeps its own state across refreshes of the conversation: the
//! parsed body, an open editor, pending reactions.

use super::super::PullRequestIntent;
use super::super::data::PullRequestData;
use super::agent::{add_to_agent_button, comment_text};
use super::body::CommentBody;
use super::editor::{EditorEvent, EditorOptions, MarkdownEditor};
use super::logic::{can_edit, preview_text, visible_body};
use super::parts::{
    Collapsed, Location, identity, location_line, open_on_github, render_collapsed, review_badge,
};
use super::reaction_bar::reaction_bar;
use super::reactions::PendingReactions;
use github::model::{Comment, CommentKind, EditableCommentKind, ReactionContent};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, Render, SharedString, Subscription, Window, div,
    prelude::*,
};

/// How a card sits on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardStyle {
    /// Bordered, with a header strip: a comment on its own.
    Card,
    /// A plain entry in a thread or a group.
    Entry,
}

pub struct CommentCard {
    data: Entity<PullRequestData>,
    comment: Comment,
    style: CardStyle,
    location: Option<Location>,
    /// A verdict given before the newest commit.
    stale: bool,
    /// Settled (a dismissed review) and folded to one line.
    collapsed: bool,
    body: CommentBody,
    editor: Option<(Entity<MarkdownEditor>, Subscription)>,
    reactions: PendingReactions,
    /// Offers "Add to agent".
    agent_button: bool,
    /// Shows ↗ to the comment on GitHub.
    github_link: bool,
}

impl EventEmitter<PullRequestIntent> for CommentCard {}

impl CommentCard {
    pub fn new(
        data: Entity<PullRequestData>,
        comment: Comment,
        style: CardStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let repo = data.read(cx).pr.repo.clone();
        let body = CommentBody::new(format!("c-{}-body", comment.id), &comment.body, &repo);
        Self {
            data,
            comment,
            style,
            location: None,
            stale: false,
            collapsed: false,
            body,
            editor: None,
            reactions: PendingReactions::default(),
            agent_button: false,
            github_link: false,
        }
    }

    /// Show where the comment sits in the code under its header.
    pub fn with_location(mut self, location: Option<Location>) -> Self {
        self.location = location;
        self
    }

    /// Offer "Add to agent" (comments on their own, not a thread's).
    pub fn with_agent_button(mut self, show: bool) -> Self {
        self.agent_button = show;
        self
    }

    /// Show ↗ to the comment on GitHub in the header.
    pub fn with_github_link(mut self, show: bool) -> Self {
        self.github_link = show;
        self
    }

    /// Start folded to one line (a dismissed review).
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    #[cfg(test)]
    pub fn comment(&self) -> &Comment {
        &self.comment
    }

    /// The conversation was read anew; what the viewer does here stays.
    pub fn set_comment(&mut self, comment: Comment, cx: &mut Context<Self>) {
        if comment == self.comment {
            return;
        }
        let repo = self.data.read(cx).pr.repo.clone();
        self.body.set_text(&comment.body, &repo, cx);
        self.comment = comment;
        cx.notify();
    }

    pub fn set_stale(&mut self, stale: bool, cx: &mut Context<Self>) {
        if self.stale != stale {
            self.stale = stale;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    #[cfg(test)]
    pub fn body(&self) -> &CommentBody {
        &self.body
    }

    fn viewer_can_edit(&self, cx: &gpui_kit::App) -> bool {
        let viewer = self.data.read(cx).viewer.as_ref().map(|v| v.login.as_str());
        can_edit(&self.comment, viewer)
    }

    pub fn start_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() || !self.viewer_can_edit(cx) {
            return;
        }
        let repo = self.data.read(cx).pr.repo.clone();
        let text = self.comment.body.clone();
        let editor = cx.new(|cx| MarkdownEditor::new(repo, &text, EditorOptions::EDIT, window, cx));
        let subscription = cx.subscribe(
            &editor,
            |this, editor, event: &EditorEvent, cx| match event {
                EditorEvent::Submit(body) => this.save(editor, body.clone(), cx),
                EditorEvent::Cancel => {
                    this.editor = None;
                    cx.notify();
                }
            },
        );
        self.editor = Some((editor, subscription));
        cx.notify();
    }

    fn save(&mut self, editor: Entity<MarkdownEditor>, body: String, cx: &mut Context<Self>) {
        let kind = match self.comment.kind {
            CommentKind::IssueComment => EditableCommentKind::IssueComment,
            CommentKind::ReviewComment => EditableCommentKind::ReviewComment,
            CommentKind::Review => return,
        };
        editor.update(cx, |editor, cx| editor.set_busy(true, cx));
        let id = self.comment.id.clone();
        let sent = body.clone();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service
                .pull_request_update_comment(pr, id, kind, sent)
                .await
        });
        cx.spawn(async move |this, cx| {
            let saved = write.await.is_ok();
            this.update(cx, |this, cx| {
                if saved {
                    // Shown until the conversation is read anew.
                    let repo = this.data.read(cx).pr.repo.clone();
                    this.body.set_text(&body, &repo, cx);
                    this.comment.body = body;
                    this.editor = None;
                } else {
                    editor.update(cx, |editor, cx| editor.set_busy(false, cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The viewer (un)reacts: shown at once, taken back when the write
    /// fails.
    pub fn react(&mut self, content: ReactionContent, reacted: bool, cx: &mut Context<Self>) {
        self.reactions
            .choose(&self.comment.reactions, content, reacted);
        cx.notify();
        let id = self.comment.id.clone();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service
                .pull_request_react(pr, Some(id), content, reacted)
                .await
        });
        cx.spawn(async move |this, cx| {
            if write.await.is_err() {
                this.update(cx, |this, cx| {
                    this.reactions.revert(content);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// The reaction bar as it stands, the viewer's pending choices applied.
    pub fn render_reactions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.reactions.shown(&self.comment.reactions);
        let can_react = self.data.read(cx).viewer.is_some();
        reaction_bar(
            &format!("c-{}", self.comment.id),
            &shown,
            can_react,
            |this: &mut Self, content, reacted, cx| this.react(content, reacted, cx),
            cx,
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = format!("c-{}", self.comment.id);
        let host = self.data.read(cx).pr.repo.host.clone();
        let editable = self.editor.is_none() && self.viewer_can_edit(cx);
        let muted = cx.theme().muted_foreground;
        let github_link = self
            .comment
            .url
            .clone()
            .filter(|_| self.github_link)
            .map(|url| open_on_github(format!("{id}-github").into(), url, cx));
        let handed = (self.agent_button && visible_body(&self.comment.body).is_some()).then(|| {
            let pr = self.data.read(cx).pr.clone();
            let text = comment_text(&pr, &self.comment, self.location.as_ref());
            let agent_id = format!("{id}-agent");
            div()
                .debug_selector({
                    let agent_id = agent_id.clone();
                    move || agent_id.clone()
                })
                .child(add_to_agent_button(agent_id.into(), text))
        });
        div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(identity(
                &id,
                self.comment.author.as_ref(),
                &host,
                self.comment.created_at,
                self.comment.url.as_deref(),
                cx,
            ))
            .children(review_badge(&id, self.comment.review_state, self.stale, cx))
            .child(div().flex_1())
            .when(editable, |row| {
                row.child(
                    div().debug_selector(|| format!("{id}-edit")).child(
                        Button::new(SharedString::from(format!("{id}-edit")))
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/pencil.svg").text_color(muted))
                            .tooltip("Edit comment")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_editing(window, cx)),
                            ),
                    ),
                )
            })
            .children(handed)
            .children(github_link)
            .children(self.render_reactions(cx))
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some((editor, _)) = &self.editor {
            return Some(editor.clone().into_any_element());
        }
        visible_body(&self.comment.body)?;
        let background = cx.theme().background;
        Some(
            self.body
                .render(background, |this: &mut Self| &mut this.body, cx),
        )
    }

    fn render_collapsed(&self, cx: &mut Context<Self>) -> AnyElement {
        let id = format!("c-{}", self.comment.id);
        let host = self.data.read(cx).pr.repo.host.clone();
        let reactions = self.render_reactions(cx);
        render_collapsed(
            Collapsed {
                id: &id,
                author: self.comment.author.as_ref(),
                host: &host,
                created_at: self.comment.created_at,
                url: self.comment.url.as_deref(),
                replies: 0,
                label: "Review dismissed",
                preview: preview_text(&self.comment.body),
                location: self.location.as_ref(),
                reactions,
            },
            |this: &mut Self, cx| {
                this.collapsed = false;
                cx.notify();
            },
            cx,
        )
    }
}

impl Render for CommentCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.collapsed {
            return self.render_collapsed(cx);
        }
        let header = self.render_header(cx);
        let location = self.location.as_ref().map(|l| location_line(l, cx));
        let content = self.render_content(cx);
        let theme = cx.theme();
        let id = SharedString::from(format!("c-{}", self.comment.id));
        match self.style {
            CardStyle::Card => div()
                .id(id)
                .min_w_0()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme.border.opacity(0.6))
                .bg(theme.background)
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_t_lg()
                        .bg(theme.muted.opacity(0.25))
                        .child(header),
                )
                .children(location.map(|l| div().px_3().pt_2().child(l)))
                .children(content.map(|c| div().px_3().py_3().child(c)))
                .into_any_element(),
            CardStyle::Entry => div()
                .id(id)
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(header)
                .children(location)
                .children(content)
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{agent_inbox, click, comment, data, review, window};
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::hours_ago;
    use github::model::{Reaction, ReviewState};
    use gpui_kit::TestAppContext;

    fn heart(count: u32, mine: bool) -> Reaction {
        Reaction {
            content: ReactionContent::Heart,
            count,
            actors: Vec::new(),
            viewer_has_reacted: mine,
        }
    }

    #[gpui_kit::test]
    fn a_reaction_shows_at_once_and_is_taken_back_when_the_write_fails(cx: &mut TestAppContext) {
        let data = data(cx, Some("me"));
        let mut liked = comment("c1", "ann", hours_ago(2));
        liked.reactions = vec![heart(1, false)];
        let (card, cx) = window(cx, move |cx| {
            CommentCard::new(data, liked, CardStyle::Card, cx)
        });
        card.update(cx, |card, cx| card.react(ReactionContent::Heart, true, cx));
        card.read_with(cx, |card, _| {
            assert_eq!(
                card.reactions.shown(&card.comment.reactions),
                [heart(2, true)]
            )
        });
        // No session service in tests: the write fails.
        cx.run_until_parked();
        card.read_with(cx, |card, cx| {
            assert_eq!(
                card.reactions.shown(&card.comment.reactions),
                [heart(1, false)]
            );
            assert!(card.data.read(cx).error.is_some(), "the failure shows");
        });
        // The pill writes the same way.
        card.update(cx, |card, cx| {
            card.data.update(cx, |d, cx| d.dismiss_error(cx))
        });
        let pill = cx.debug_bounds("c-c1-reaction-Heart").expect("a pill");
        cx.simulate_click(pill.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        card.read_with(cx, |card, cx| assert!(card.data.read(cx).error.is_some()));
    }

    #[gpui_kit::test]
    fn the_viewer_edits_their_own_comment_in_place(cx: &mut TestAppContext) {
        let data = data(cx, Some("ann"));
        let (card, cx) = window(cx, move |cx| {
            CommentCard::new(
                data,
                comment("c1", "ann", hours_ago(2)),
                CardStyle::Card,
                cx,
            )
        });
        let edit = cx.debug_bounds("c-c1-edit").expect("a pencil");
        cx.simulate_click(edit.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        card.read_with(cx, |card, _| assert!(card.is_editing()));
        assert!(
            cx.debug_bounds("c-c1-edit").is_none(),
            "no pencil while editing"
        );
        cx.simulate_keystrokes("escape");
        card.read_with(cx, |card, _| assert!(!card.is_editing()));
    }

    #[gpui_kit::test]
    fn add_to_agent_hands_the_comment_to_the_draft(cx: &mut TestAppContext) {
        let data = data(cx, None);
        let (_, cx) = window(cx, move |cx| {
            CommentCard::new(
                data,
                comment("c1", "ann", hours_ago(2)),
                CardStyle::Card,
                cx,
            )
            .with_agent_button(true)
        });
        let inbox = agent_inbox(cx);
        click(cx, "c-c1-agent");
        assert_eq!(inbox.borrow().len(), 1);
        assert!(inbox.borrow()[0].starts_with("Comment by @ann on stippi/code-assistant#290"));
    }

    #[gpui_kit::test]
    fn others_comments_and_review_summaries_have_no_pencil(cx: &mut TestAppContext) {
        let data = data(cx, Some("ann"));
        let (_, cx) = window(cx, move |cx| {
            CommentCard::new(
                data,
                review("r1", "ann", ReviewState::Approved, hours_ago(2)),
                CardStyle::Card,
                cx,
            )
        });
        assert!(cx.debug_bounds("c-r1-edit").is_none());
    }

    #[gpui_kit::test]
    fn a_dismissed_review_unfolds_and_mounts_its_body_then(cx: &mut TestAppContext) {
        let data = data(cx, None);
        let (card, cx) = window(cx, move |cx| {
            CommentCard::new(
                data,
                review("r1", "bob", ReviewState::Dismissed, hours_ago(2)),
                CardStyle::Card,
                cx,
            )
            .collapsed(true)
        });
        card.read_with(cx, |card, _| assert!(!card.body().is_mounted()));
        let expand = cx.debug_bounds("c-r1-expand").expect("folded");
        cx.simulate_click(expand.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        card.read_with(cx, |card, _| assert!(card.body().is_mounted()));
    }
}
