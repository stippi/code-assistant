//! The floating composer at the detail page's bottom right: a round button
//! (with the number of pending line comments) opening a popover to comment
//! on the pull request or to review it. Ported from T3 Code's
//! `PullRequestComposer`, `PullRequestCommentForm` and
//! `PullRequestReviewForm`.
//!
//! **Comment**: ⌘Enter or "Comment" posts; "Close with comment" (two
//! clicks) or "Reopen with comment" post and then act. **Review**: a
//! summary and a verdict (those the viewer may give) sent with the pending
//! line comments of [`review_drafts`]. Both texts live as long as the page,
//! so dismissing the popover keeps them; a sent text is cleared.

use super::actions::{Confirmable, InlineConfirm, can};
use super::data::PullRequestData;
use super::review_draft::{ReviewDrafts, review_drafts};
use crate::shared::dismiss::{TriggerBounds, dismiss_layer};
use crate::shared::segmented_switch::segmented_switch;
use github::model::{Action, Detail, PullRequestAction, PullRequestState, ReviewVerdict};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable};
use gpui_kit::{
    Anchor, AnyElement, Context, Edges, Entity, Render, Subscription, Task, Window, anchored,
    deferred, div, prelude::*, px, svg,
};
use std::time::Duration;

/// How long "Close with comment" waits for its confirming click.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerMode {
    Comment,
    Review,
}

/// What follows a comment besides posting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    Close,
    Reopen,
}

/// Whether the viewer may comment, and the verdicts they may give.
pub fn offered(detail: &Detail) -> (bool, Vec<ReviewVerdict>) {
    let verdicts = ReviewVerdict::ALL
        .into_iter()
        .filter(|v| detail.viewer_permissions.verdicts.contains(v))
        .collect();
    (detail.viewer_permissions.comment, verdicts)
}

pub fn follow_up(detail: &Detail) -> Option<FollowUp> {
    match detail.state {
        PullRequestState::Open if can(detail, PullRequestAction::Close) => Some(FollowUp::Close),
        PullRequestState::Closed if can(detail, PullRequestAction::Reopen) => {
            Some(FollowUp::Reopen)
        }
        _ => None,
    }
}

/// An approval needs no words; another verdict needs a summary or line
/// comments.
pub fn can_submit_review(verdict: ReviewVerdict, summary: &str, pending: usize) -> bool {
    verdict == ReviewVerdict::Approve || !summary.trim().is_empty() || pending > 0
}

fn verdict_label(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Comment => "Comment",
        ReviewVerdict::Approve => "Approve",
        ReviewVerdict::RequestChanges => "Request changes",
    }
}

fn verdict_icon(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Comment => "icons/message_square.svg",
        ReviewVerdict::Approve => "icons/check.svg",
        ReviewVerdict::RequestChanges => "icons/circle_x.svg",
    }
}

/// What is being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sending {
    Comment,
    CommentAnd(FollowUp),
    Review,
}

/// The two texts, made when the composer first opens (they need a window).
struct Forms {
    comment: Entity<TextareaState>,
    summary: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

pub struct Composer {
    data: Entity<PullRequestData>,
    drafts: Entity<ReviewDrafts>,
    open: bool,
    /// The form asked for; permissions may force the other.
    requested: ComposerMode,
    forms: Option<Forms>,
    verdict: ReviewVerdict,
    sending: Option<Sending>,
    confirm: InlineConfirm,
    trigger: TriggerBounds,
    _confirm_timeout: Option<Task<()>>,
    _sending: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(data: Entity<PullRequestData>, cx: &mut Context<Self>) -> Self {
        let drafts = review_drafts(cx);
        let subscriptions = vec![
            cx.observe(&data, |_, _, cx| cx.notify()),
            cx.observe(&drafts, |_, _, cx| cx.notify()),
        ];
        Self {
            data,
            drafts,
            open: false,
            requested: ComposerMode::Comment,
            forms: None,
            verdict: ReviewVerdict::Comment,
            sending: None,
            confirm: InlineConfirm::default(),
            trigger: TriggerBounds::default(),
            _confirm_timeout: None,
            _sending: None,
            _subscriptions: subscriptions,
        }
    }

    fn forms(&mut self, window: &mut Window, cx: &mut Context<Self>) -> &Forms {
        if self.forms.is_none() {
            let comment = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 12)
                    .placeholder("Leave a comment")
            });
            let summary = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 12)
                    .placeholder("Summarize your review")
            });
            let subscriptions = vec![
                cx.subscribe_in(&comment, window, |this, _, event, window, cx| {
                    if matches!(
                        event,
                        InputEvent::PressEnter {
                            secondary: true,
                            ..
                        }
                    ) {
                        this.send_comment(None, window, cx);
                    } else if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
                cx.subscribe(&summary, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            ];
            self.forms = Some(Forms {
                comment,
                summary,
                _subscriptions: subscriptions,
            });
        }
        self.forms.as_ref().expect("made above")
    }

    /// One of the texts, empty before the composer first opened.
    fn text(&self, input: fn(&Forms) -> &Entity<TextareaState>, cx: &gpui_kit::App) -> String {
        self.forms
            .as_ref()
            .map(|forms| input(forms).read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn pending(&self, cx: &gpui_kit::App) -> usize {
        self.drafts.read(cx).count(&self.data.read(cx).pr)
    }

    /// The form shown: the asked one where the viewer may use it.
    fn mode(&self, comment: bool, verdicts: &[ReviewVerdict]) -> ComposerMode {
        match (comment, verdicts.is_empty()) {
            (true, true) => ComposerMode::Comment,
            (false, _) => ComposerMode::Review,
            (true, false) => self.requested,
        }
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        // A review under way is what the user comes back to.
        let started = self.pending(cx) > 0 || !self.text(|f| &f.summary, cx).trim().is_empty();
        self.requested = if started {
            ComposerMode::Review
        } else {
            ComposerMode::Comment
        };
        self.open = true;
        self.focus_form(window, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.confirm.disarm();
        cx.notify();
    }

    fn select_mode(&mut self, mode: ComposerMode, window: &mut Window, cx: &mut Context<Self>) {
        self.requested = mode;
        self.focus_form(window, cx);
        cx.notify();
    }

    fn focus_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let requested = self.requested;
        let forms = self.forms(window, cx);
        let input = match requested {
            ComposerMode::Comment => forms.comment.clone(),
            ComposerMode::Review => forms.summary.clone(),
        };
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Post the comment, then close or reopen when asked.
    fn send_comment(
        &mut self,
        then: Option<FollowUp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = self.text(|f| &f.comment, cx).trim().to_owned();
        if body.is_empty() || self.sending.is_some() {
            return;
        }
        self.sending = Some(match then {
            Some(follow_up) => Sending::CommentAnd(follow_up),
            None => Sending::Comment,
        });
        cx.notify();
        let data = self.data.clone();
        let posted = PullRequestData::write(&data, cx, move |service, pr| async move {
            service.pull_request_comment(pr, body).await
        });
        self._sending = Some(cx.spawn_in(window, async move |this, cx| {
            let posted = posted.await.is_ok();
            if posted && let Some(follow_up) = then {
                let action = match follow_up {
                    FollowUp::Close => Action::Close,
                    FollowUp::Reopen => Action::Reopen,
                };
                let acted = cx.update(|_, cx| {
                    PullRequestData::write(&data, cx, move |service, pr| async move {
                        service.pull_request_action(pr, action).await
                    })
                });
                if let Ok(acted) = acted {
                    let _ = acted.await;
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.sending = None;
                if posted {
                    let comment = this.forms(window, cx).comment.clone();
                    comment.update(cx, |input, cx| input.set_value("", window, cx));
                    this.close(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// "Close with comment" takes two clicks.
    fn click_close_with_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.click(Confirmable::Close) {
            self._confirm_timeout = None;
            self.send_comment(Some(FollowUp::Close), window, cx);
        } else {
            self._confirm_timeout = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CONFIRM_TIMEOUT).await;
                this.update(cx, |this, cx| {
                    this.confirm.disarm();
                    cx.notify();
                })
                .ok();
            }));
            cx.notify();
        }
    }

    /// Send the review with the pending line comments; those added while
    /// it is under way stay pending, and so does a summary edited meanwhile.
    fn submit_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending.is_some() {
            return;
        }
        let summary = self.text(|f| &f.summary, cx);
        let pr = self.data.read(cx).pr.clone();
        let (review, ids) =
            self.drafts
                .read(cx)
                .submission(&pr, self.verdict, summary.trim().to_owned());
        self.sending = Some(Sending::Review);
        cx.notify();
        let sent = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_submit_review(pr, review).await
        });
        self._sending = Some(cx.spawn_in(window, async move |this, cx| {
            let sent = sent.await.is_ok();
            this.update_in(cx, |this, window, cx| {
                this.sending = None;
                if sent {
                    this.drafts
                        .update(cx, |drafts, cx| drafts.remove_submitted(&pr, &ids, cx));
                    if this.text(|f| &f.summary, cx) == summary {
                        let input = this.forms(window, cx).summary.clone();
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    this.close(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn discard_pending(&mut self, cx: &mut Context<Self>) {
        let pr = self.data.read(cx).pr.clone();
        self.drafts.update(cx, |drafts, cx| {
            let ids: Vec<u64> = drafts.comments(&pr).iter().map(|c| c.id).collect();
            drafts.remove_submitted(&pr, &ids, cx);
        });
    }

    fn render_trigger(&self, pending: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("pr-composer")
            .debug_selector(|| "pr-composer".into())
            .relative()
            .size(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(self.trigger.probe())
            .child(
                svg()
                    .size(px(16.))
                    .path("icons/message_square.svg")
                    .text_color(theme.foreground),
            )
            .when(pending > 0, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .h(px(16.))
                        .min_w(px(16.))
                        .px_1()
                        .rounded_full()
                        .bg(theme.info)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.))
                        .text_color(gpui_kit::white())
                        .child(pending.to_string()),
                )
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
    }

    fn render_comment_form(
        &self,
        detail: &Detail,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let empty = input.read(cx).value().trim().is_empty();
        let busy = self.sending.is_some();
        let armed = self.confirm.is_armed(Confirmable::Close);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Textarea::new(input))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .children(follow_up(detail).map(|follow_up| {
                        match follow_up {
                            FollowUp::Close => Button::new("pr-comment-close")
                                .outline()
                                .danger()
                                .xsmall()
                                .icon(Icon::default().path("icons/git_pull_request_closed.svg"))
                                .label(if self.sending == Some(Sending::CommentAnd(follow_up)) {
                                    "Closing…"
                                } else if armed {
                                    "Confirm close?"
                                } else {
                                    "Close with comment"
                                })
                                .disabled(empty || busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.click_close_with_comment(window, cx)
                                }))
                                .into_any_element(),
                            FollowUp::Reopen => Button::new("pr-comment-reopen")
                                .outline()
                                .xsmall()
                                .icon(Icon::default().path("icons/git_pull_request.svg"))
                                .label(if self.sending == Some(Sending::CommentAnd(follow_up)) {
                                    "Reopening…"
                                } else {
                                    "Reopen with comment"
                                })
                                .disabled(empty || busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.send_comment(Some(FollowUp::Reopen), window, cx)
                                }))
                                .into_any_element(),
                        }
                    }))
                    .child(
                        div().debug_selector(|| "pr-comment-post".into()).child(
                            Button::new("pr-comment-post")
                                .outline()
                                .xsmall()
                                .icon(Icon::default().path("icons/send.svg"))
                                .label(if self.sending == Some(Sending::Comment) {
                                    "Posting…"
                                } else {
                                    "Comment"
                                })
                                .tooltip("⌘↵")
                                .disabled(empty || busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_comment(None, window, cx)
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_review_form(
        &self,
        verdicts: &[ReviewVerdict],
        pending: usize,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let verdict = if verdicts.contains(&self.verdict) {
            self.verdict
        } else {
            verdicts[0]
        };
        let summary = input.read(cx).value().to_string();
        let ready = can_submit_review(verdict, &summary, pending) && self.sending.is_none();
        let composer = cx.entity().downgrade();
        let choices = verdicts.to_vec();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Textarea::new(input))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(choices.len() > 1, |row| {
                        row.child(
                            Button::new("pr-review-verdict")
                                .outline()
                                .xsmall()
                                .icon(Icon::default().path(verdict_icon(verdict)))
                                .label(verdict_label(verdict))
                                .dropdown_caret(true)
                                .dropdown_menu_with_anchor(
                                    Anchor::BottomLeft,
                                    move |menu, _, _| {
                                        choices.iter().fold(menu, |menu, &choice| {
                                            let composer = composer.clone();
                                            menu.item(
                                                PopupMenuItem::new(verdict_label(choice))
                                                    .icon(
                                                        Icon::default().path(verdict_icon(choice)),
                                                    )
                                                    .checked(choice == verdict)
                                                    .on_click(move |_, _, cx| {
                                                        composer
                                                            .update(cx, |this, cx| {
                                                                this.verdict = choice;
                                                                cx.notify();
                                                            })
                                                            .ok();
                                                    }),
                                            )
                                        })
                                    },
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div().debug_selector(|| "pr-review-submit".into()).child(
                            Button::new("pr-review-submit")
                                .primary()
                                .xsmall()
                                .label(if self.sending == Some(Sending::Review) {
                                    "Submitting…"
                                } else {
                                    "Submit review"
                                })
                                .disabled(!ready)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.verdict = verdict;
                                    this.submit_review(window, cx)
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_popover(
        &mut self,
        detail: &Detail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (can_comment, verdicts) = offered(detail);
        let verdicts = verdicts.as_slice();
        let pending = self.pending(cx);
        let mode = self.mode(can_comment, verdicts);
        let (comment, summary) = {
            let forms = self.forms(window, cx);
            (forms.comment.clone(), forms.summary.clone())
        };
        let composer = cx.entity().downgrade();
        let title: AnyElement = if can_comment && !verdicts.is_empty() {
            segmented_switch(
                "pr-composer-mode",
                &[
                    (ComposerMode::Comment, "Comment"),
                    (ComposerMode::Review, "Review"),
                ],
                mode,
                move |mode, window, cx| {
                    composer
                        .update(cx, |this, cx| this.select_mode(mode, window, cx))
                        .ok();
                },
                window,
                cx,
            )
            .into_any_element()
        } else {
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(match mode {
                    ComposerMode::Comment => "Comment on pull request",
                    ComposerMode::Review => "Review pull request",
                })
                .into_any_element()
        };
        let body = match mode {
            ComposerMode::Comment => self.render_comment_form(detail, &comment, cx),
            ComposerMode::Review => self.render_review_form(verdicts, pending, &summary, cx),
        };
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        div()
            .id("pr-composer-popover")
            .occlude()
            .w(px(420.))
            .mb_2()
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                this.close(cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(title)
                    .when(mode == ComposerMode::Review && pending > 0, |row| {
                        row.child(div().text_xs().text_color(muted).child(format!(
                            "{pending} pending {}",
                            if pending == 1 { "comment" } else { "comments" }
                        )))
                        .child(
                            Button::new("pr-review-discard")
                                .ghost()
                                .xsmall()
                                .icon(Icon::default().path("icons/trash.svg"))
                                .tooltip("Discard pending line comments")
                                .disabled(self.sending.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.discard_pending(cx))),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("pr-composer-close")
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/close.svg"))
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(detail) = self.data.read(cx).detail.clone() else {
            return div().into_any_element();
        };
        let (can_comment, verdicts) = offered(&detail);
        if !can_comment && verdicts.is_empty() {
            return div().into_any_element();
        }
        let pending = self.pending(cx);
        let mut root = div()
            .relative()
            .flex()
            .child(self.render_trigger(pending, cx));
        if self.open {
            let popover = self.render_popover(&detail, window, cx);
            root = root
                .child(dismiss_layer(
                    &self.trigger,
                    window,
                    cx.listener(|this, _, _, cx| this.close(cx)),
                ))
                .child(deferred(
                    anchored()
                        .anchor(Anchor::BottomRight)
                        .snap_to_window_with_margin(Edges::all(px(8.)))
                        .child(popover),
                ));
        }
        root.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{detail, detail_with, repo};
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::sync::Arc;

    #[test]
    fn permissions_decide_the_forms() {
        let commenter = detail(1);
        assert_eq!(offered(&commenter), (true, Vec::new()));
        let reviewer = detail_with(1, |d| {
            d.viewer_permissions.verdicts =
                vec![ReviewVerdict::RequestChanges, ReviewVerdict::Comment]
        });
        // In the order the form offers them.
        assert_eq!(
            offered(&reviewer).1,
            [ReviewVerdict::Comment, ReviewVerdict::RequestChanges]
        );
    }

    #[test]
    fn a_comment_can_close_an_open_pull_request_or_reopen_a_closed_one() {
        assert_eq!(follow_up(&detail(1)), Some(FollowUp::Close));
        let closed = detail_with(1, |d| d.state = PullRequestState::Closed);
        assert_eq!(follow_up(&closed), Some(FollowUp::Reopen));
        let merged = detail_with(1, |d| d.state = PullRequestState::Merged);
        assert_eq!(follow_up(&merged), None);
        let not_allowed = detail_with(1, |d| d.viewer_permissions.actions.clear());
        assert_eq!(follow_up(&not_allowed), None);
    }

    #[test]
    fn only_an_approval_goes_without_words() {
        assert!(can_submit_review(ReviewVerdict::Approve, "", 0));
        assert!(!can_submit_review(ReviewVerdict::Comment, "  ", 0));
        assert!(can_submit_review(ReviewVerdict::Comment, "", 2));
        assert!(can_submit_review(
            ReviewVerdict::RequestChanges,
            "Rename it",
            0
        ));
    }

    fn composer(
        detail: Detail,
        cx: &mut TestAppContext,
    ) -> (Entity<Composer>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| {
                let data = cx.new(|cx| PullRequestData::new(repo().pull_request(1), cx));
                data.update(cx, |data, _| data.detail = Some(Arc::new(detail)));
                cx.new(|cx| Composer::new(data, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui_kit::test]
    fn a_started_review_reopens_on_the_review_form(cx: &mut TestAppContext) {
        let reviewer = detail_with(1, |d| {
            d.viewer_permissions.verdicts = ReviewVerdict::ALL.to_vec()
        });
        let (view, cx) = composer(reviewer, cx);
        let bounds = cx.debug_bounds("pr-composer").unwrap();
        cx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("pr-comment-post").is_some(),
            "comment first"
        );

        cx.simulate_input("Needs a test");
        view.update_in(cx, |view, window, cx| {
            view.select_mode(ComposerMode::Review, window, cx)
        });
        cx.simulate_input("Mostly fine");
        view.update_in(cx, |view, window, cx| view.toggle(window, cx));
        view.update_in(cx, |view, window, cx| view.toggle(window, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-review-submit").is_some());
        view.read_with(cx, |view, cx| {
            assert_eq!(view.text(|f| &f.comment, cx), "Needs a test");
            assert_eq!(view.text(|f| &f.summary, cx), "Mostly fine");
        });
    }

    #[gpui_kit::test]
    fn a_comment_that_did_not_post_is_kept(cx: &mut TestAppContext) {
        let (view, cx) = composer(detail(1), cx);
        view.update_in(cx, |view, window, cx| view.toggle(window, cx));
        cx.simulate_input("Looks good");
        view.update_in(cx, |view, window, cx| view.send_comment(None, window, cx));
        cx.run_until_parked();
        // No session service: nothing reaches GitHub.
        view.read_with(cx, |view, cx| {
            assert!(view.open);
            assert_eq!(view.text(|f| &f.comment, cx), "Looks good");
            assert!(view.data.read(cx).error.is_some());
        });
    }
}
