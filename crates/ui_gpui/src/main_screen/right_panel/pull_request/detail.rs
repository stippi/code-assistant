//! One pull request: the header, the tab row and the shown tab's body.
//! What the page knows lives in [`PullRequestData`], which the header and
//! each tab body ([`SummaryTab`], [`TimelineTab`], [`CodeTab`]) observe;
//! the view passes their intents on and carries out the writes on the
//! pull request (actions, updates, refresh).

use super::PullRequestIntent;
use super::actions::{Confirmable, InlineConfirm, workflow_approvals};
use super::code::CodeTab;
use super::composer::Composer;
use super::data::{Notice, PullRequestData};
use super::header::CONFIRM_TIMEOUT;
use super::header::DetailHeader;
use super::preferences;
use super::presentation::merge_method_label;
use super::stack::{StackActionKind, StackPlan, StackRequest, confirmation, layer_row};
use super::summary::SummaryTab;
use super::tab_bar::{approve_workflows_slot, checks_slot, render_tab_row, timeline_slot};
use super::timeline::{OpenCommit, TimelineTab};
use anyhow::Context as _;
use code_assistant_core::pull_requests::{CheckoutPlace, PullRequestCheckout};
use github::PullRequestRef;
use github::model::StackAction;
use github::model::{Action, ListEntry, PullRequestUpdate};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, button::Button, button::ButtonVariants};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, ListState, Pixels, Render, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Summary,
    Timeline,
    Code,
}

impl DetailTab {
    pub const ALL: &[(DetailTab, &'static str)] = &[
        (DetailTab::Summary, "Summary"),
        (DetailTab::Timeline, "Timeline"),
        (DetailTab::Code, "Code"),
    ];
}

pub struct DetailView {
    pr: PullRequestRef,
    data: Entity<PullRequestData>,
    tab: DetailTab,
    /// The header is condensed (see [`condenses`]).
    condensed: bool,
    header: Entity<DetailHeader>,
    summary: Entity<SummaryTab>,
    timeline: Entity<TimelineTab>,
    code: Entity<CodeTab>,
    composer: Entity<Composer>,
    /// The tab row's "Approve workflows to run" waits for its second click.
    confirm: InlineConfirm,
    _confirm_timeout: Option<Task<()>>,
    approving: bool,
    /// The checks popover lists the completed checks too.
    checks_show_all: bool,
    /// Merging or rebasing the stack waits for confirmation.
    stack_confirm: Option<StackActionKind>,
    /// A stack action runs.
    stack_acting: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for DetailView {}

impl DetailView {
    /// `seed` is the listing row it was opened from; `show_back` whether
    /// there is a list to go back to. Reads the pull request and polls its
    /// checks while shown.
    pub fn new(
        pr: PullRequestRef,
        seed: Option<ListEntry>,
        show_back: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let data = cx.new(|cx| PullRequestData::new(pr.clone(), cx));
        data.update(cx, |data, cx| {
            data.load(cx);
            data.set_polling(true, cx);
        });
        let merge_method = preferences::read(cx).merge_method(&pr.repo);
        let header = cx.new(|_| DetailHeader::new(pr.clone(), seed, show_back, merge_method));
        let summary = cx.new(|cx| SummaryTab::new(data.clone(), cx));
        let timeline = cx.new(|_| TimelineTab::new(data.clone()));
        let code = cx.new(|cx| CodeTab::new(data.clone(), cx));
        let composer = cx.new(|cx| Composer::new(data.clone(), cx));
        let subscriptions = vec![
            cx.observe(&data, |this, _, cx| this.sync_header(cx)),
            cx.subscribe(&header, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            cx.subscribe(
                &header,
                |this, _, request: &StackRequest, cx| match *request {
                    StackRequest::Retry => this.data.update(cx, |data, cx| {
                        data.read(
                            code_assistant_core::pull_requests::PullRequestPart::Stack,
                            cx,
                        )
                    }),
                    StackRequest::Confirm(kind) if !this.stack_acting => {
                        this.stack_confirm = Some(kind);
                        cx.notify();
                    }
                    StackRequest::Confirm(_) => {}
                },
            ),
            cx.subscribe(&summary, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            cx.subscribe(&timeline, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            // A commit clicked in the timeline: its diff in the Code tab.
            cx.subscribe(&timeline, |this, _, OpenCommit(oid): &OpenCommit, cx| {
                this.code
                    .update(cx, |code, cx| code.select_commit(Some(oid.clone()), cx));
                this.select_tab(DetailTab::Code, cx);
            }),
            cx.subscribe(&code, |_, _, intent: &PullRequestIntent, cx| {
                cx.emit(intent.clone())
            }),
            // The tab row shows the timeline's order.
            cx.observe(&timeline, |_, _, cx| cx.notify()),
        ];
        Self {
            pr,
            data,
            tab: DetailTab::default(),
            condensed: false,
            header,
            summary,
            timeline,
            code,
            composer,
            confirm: InlineConfirm::default(),
            _confirm_timeout: None,
            approving: false,
            checks_show_all: false,
            stack_confirm: None,
            stack_acting: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn pr(&self) -> &PullRequestRef {
        &self.pr
    }

    #[cfg(test)]
    pub fn data(&self) -> &Entity<PullRequestData> {
        &self.data
    }

    fn sync_header(&mut self, cx: &mut Context<Self>) {
        let (detail, checks_state, refreshing, stack) = {
            let data = self.data.read(cx);
            (
                data.detail.clone(),
                data.checks_state(),
                data.refreshing,
                data.stack.clone(),
            )
        };
        self.header.update(cx, |header, cx| {
            header.set_data(detail, checks_state, refreshing, cx);
            header.set_stack(stack, cx);
        });
        cx.notify();
    }

    /// Merge the stack through the shown layer, or rebase it from its
    /// top, as confirmed; then every layer is read anew.
    fn run_stack_action(&mut self, kind: StackActionKind, cx: &mut Context<Self>) {
        self.stack_confirm = None;
        let (detail, stack) = {
            let data = self.data.read(cx);
            (data.detail.clone(), data.stack.stack.clone())
        };
        let (Some(detail), Some(stack)) = (detail, stack) else {
            return;
        };
        let Some(plan) = StackPlan::of(&stack, detail.number) else {
            return;
        };
        let (target, heads) = plan.target(kind);
        let action = match kind {
            StackActionKind::Merge => {
                StackAction::Merge(self.header.read(cx).merge_method(&detail))
            }
            StackActionKind::Rebase => StackAction::Rebase,
        };
        let (pr, number) = (self.pr.repo.pull_request(target), stack.number);
        self.stack_acting = true;
        let write = PullRequestData::write(&self.data, cx, move |service, _| async move {
            service
                .pull_request_stack_action(pr, number, heads, action)
                .await
                .context(match kind {
                    StackActionKind::Merge => "Could not merge the stack",
                    StackActionKind::Rebase => "Could not rebase the stack",
                })
        });
        let data = self.data.downgrade();
        cx.spawn(async move |this, cx| {
            let done = write.await.is_ok();
            this.update(cx, |this, cx| {
                this.stack_acting = false;
                cx.notify();
            })
            .ok();
            if done {
                data.update(cx, |data, cx| {
                    data.notice = Some(match kind {
                        StackActionKind::Merge => Notice {
                            title: "Stack merge request completed".into(),
                            detail: "GitHub merged the stack or added it to its merge queue."
                                .into(),
                            warning: false,
                        },
                        StackActionKind::Rebase => Notice {
                            title: "Stack rebased".into(),
                            detail: "".into(),
                            warning: false,
                        },
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// "Merge 2 pull requests?": what the stack action does, the layers it
    /// works on, Cancel and the action.
    fn render_stack_confirm(
        &self,
        kind: StackActionKind,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let data = self.data.read(cx);
        let detail = data.detail.clone()?;
        let stack = data.stack.stack.clone()?;
        let plan = StackPlan::of(&stack, detail.number)?;
        let method = merge_method_label(self.header.read(cx).merge_method(&detail)).to_lowercase();
        let (title, description) = confirmation(kind, &plan, &stack.base, &method, "GitHub");
        let theme = cx.theme();
        let rows = plan.layers(kind).iter().map(|layer| {
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme.muted.opacity(0.5))
                .child(layer_row(layer, false, true, cx))
        });
        let action = match kind {
            StackActionKind::Merge => "Merge stack",
            StackActionKind::Rebase => "Rebase stack",
        };
        let card = div()
            .id("pr-stack-confirm-card")
            .w_full()
            .max_w(px(448.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_lg()
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation()
            })
            .child(
                div()
                    .text_base()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(description),
            )
            .child(
                div()
                    .id("pr-stack-confirm-layers")
                    .max_h(px(192.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pr-stack-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stack_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div().debug_selector(|| "pr-stack-confirm".into()).child(
                            Button::new("pr-stack-confirm")
                                .primary()
                                .small()
                                .label(action)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run_stack_action(kind, cx)
                                })),
                        ),
                    ),
            );
        Some(
            div()
                .id("pr-stack-confirm")
                .absolute()
                .inset_0()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::black().opacity(0.3))
                .on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.stack_confirm = None;
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }

    /// Shown again (read anew) or hidden (the polling pauses).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.data.update(cx, |data, cx| {
            if visible {
                data.load(cx);
            }
            data.set_polling(visible, cx);
        });
    }

    pub fn select_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn tab(&self) -> DetailTab {
        self.tab
    }

    /// Merge, close, … the pull request; the primary control waits meanwhile.
    /// Whether it went through comes out of the task.
    pub fn act(&mut self, action: Action, cx: &mut Context<Self>) -> Task<bool> {
        self.header
            .update(cx, |header, cx| header.set_pending(true, cx));
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_action(pr, action).await
        });
        cx.spawn(async move |this, cx| {
            let done = write.await.is_ok();
            this.update(cx, |this, cx| {
                this.header
                    .update(cx, |header, cx| header.set_pending(false, cx))
            })
            .ok();
            done
        })
    }

    /// Rewrite the title, body or maintainer edits; a refused title is
    /// taken back.
    pub fn update(&mut self, update: PullRequestUpdate, cx: &mut Context<Self>) {
        let retitles = update.title.is_some();
        let write = PullRequestData::write(&self.data, cx, move |service, pr| async move {
            service.pull_request_update(pr, update).await
        });
        cx.spawn(async move |this, cx| {
            if write.await.is_err() && retitles {
                this.update(cx, |this, cx| {
                    this.header
                        .update(cx, |header, cx| header.title_rejected(cx))
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        PullRequestData::refresh(&self.data, cx);
    }

    /// The first click on "Approve workflows to run" arms it for a while,
    /// the second approves.
    pub fn click_approve_workflows(&mut self, cx: &mut Context<Self>) {
        if self.approving {
            return;
        }
        if !self.confirm.click(Confirmable::ApproveWorkflows) {
            self._confirm_timeout = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CONFIRM_TIMEOUT).await;
                this.update(cx, |this, cx| this.disarm(cx)).ok();
            }));
            cx.notify();
            return;
        }
        self._confirm_timeout = None;
        self.approving = true;
        let write = PullRequestData::write(&self.data, cx, |service, pr| async move {
            service
                .pull_request_action(pr, Action::ApproveWorkflows)
                .await
                .context("Could not approve workflows")
        });
        cx.spawn(async move |this, cx| {
            write.await.ok();
            this.update(cx, |this, cx| {
                this.approving = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn checks_show_all(&self) -> bool {
        self.checks_show_all
    }

    pub fn toggle_checks_show_all(&mut self, cx: &mut Context<Self>) {
        self.checks_show_all = !self.checks_show_all;
        cx.notify();
    }

    pub fn disarm(&mut self, cx: &mut Context<Self>) {
        self._confirm_timeout = None;
        if self.confirm.disarm() {
            cx.notify();
        }
    }

    /// What the tab row shows on its right for the shown tab.
    fn tab_slot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.tab {
            DetailTab::Summary => {
                let data = self.data.read(cx);
                let approvals = data.detail.as_deref().map_or(0, |detail| {
                    workflow_approvals(detail, data.checks.as_deref())
                });
                if approvals > 0 || self.approving {
                    let armed = self.confirm.is_armed(Confirmable::ApproveWorkflows);
                    let view = cx.entity().downgrade();
                    return Some(approve_workflows_slot(armed, self.approving, view, cx));
                }
                let checks = data.current_checks().to_vec();
                let state = data.checks_state();
                checks_slot(&checks, state, cx.entity().downgrade(), cx)
            }
            DetailTab::Timeline => {
                let activity = self.data.read(cx).activity.clone();
                Some(timeline_slot(activity.as_deref(), &self.timeline, cx))
            }
            DetailTab::Code => None,
        }
    }

    pub fn set_checking_out(&mut self, checking_out: bool, cx: &mut Context<Self>) {
        self.header
            .update(cx, |header, cx| header.set_checking_out(checking_out, cx));
    }

    /// Say what a checkout came to, in T3 Code's words.
    pub fn checked_out(
        &mut self,
        place: CheckoutPlace,
        done: &anyhow::Result<PullRequestCheckout>,
        cx: &mut Context<Self>,
    ) {
        self.data.update(cx, |data, cx| {
            match done {
                Ok(done) => {
                    data.error = None;
                    data.notice = Some(checkout_notice(place, done));
                }
                Err(error) => {
                    data.notice = None;
                    data.error = Some(
                        format!(
                            "Could not prepare the pull request checkout: {error:#}. Check that \
                             the branch still exists and is not checked out elsewhere, then \
                             try again."
                        )
                        .into(),
                    );
                }
            }
            cx.notify();
        });
    }

    fn render_notice(&self, notice: Notice, cx: &mut Context<Self>) -> AnyElement {
        let color = match notice.warning {
            true => cx.theme().warning,
            false => cx.theme().info,
        };
        div()
            .debug_selector(|| "pr-notice".into())
            .flex_none()
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .bg(color.opacity(0.08))
            .border_b_1()
            .border_color(color.opacity(0.25))
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(color)
                            .child(notice.title),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(notice.detail),
                    ),
            )
            .child(
                Button::new("pr-notice-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path("icons/close.svg"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.data.update(cx, |data, cx| data.dismiss_notice(cx))
                    })),
            )
            .into_any_element()
    }

    /// The last failure, until it is dismissed or something succeeds.
    fn render_error(&self, error: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        div()
            .debug_selector(|| "pr-error".into())
            .flex_none()
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .bg(danger.opacity(0.08))
            .border_b_1()
            .border_color(danger.opacity(0.25))
            .text_xs()
            .text_color(danger)
            .child(div().flex_1().min_w_0().child(error))
            .child(
                Button::new("pr-error-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().path("icons/close.svg"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.data.update(cx, |data, cx| data.dismiss_error(cx))
                    })),
            )
            .into_any_element()
    }
}

/// T3 Code's toast after a checkout; a warning when it did not land on the
/// pull request's head.
fn checkout_notice(place: CheckoutPlace, done: &PullRequestCheckout) -> Notice {
    let (title, detail) = match (done.on_head, place) {
        (false, _) => (
            "Checked out, but not confirmed on the latest commit",
            "This checkout could not be confirmed at the pull request's latest commit, so its \
             code may differ. Local commits or uncommitted work keep a checkout where it is.",
        ),
        (true, CheckoutPlace::Here) => (
            "Checked out here",
            "This repository is on the pull request's branch.",
        ),
        (true, CheckoutPlace::Worktree) => (
            "Checked out",
            "The pull request is in its own worktree, with a session open on it.",
        ),
    };
    Notice {
        title: title.into(),
        detail: format!("{detail} ({}, {})", done.branch, done.path.display()).into(),
        warning: !done.on_head,
    }
}

/// How far a tab's list is scrolled, and how far it could be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabScroll {
    pub offset: Pixels,
    pub max: Pixels,
}

impl TabScroll {
    pub fn of(list: &ListState) -> Self {
        Self {
            offset: -list.scroll_px_offset_for_scrollbar().y,
            max: list.max_offset_for_scrollbar().y,
        }
    }
}

/// Scrolled this far, the tab's content has left the header's room.
const CONDENSE_AFTER: Pixels = px(24.);
/// More than the header gives up when it condenses (title, meta, branch
/// and behind-by rows become one line).
const CONDENSE_ROOM: Pixels = px(160.);

/// Whether the header is condensed next, given whether it is now. It
/// condenses past [`CONDENSE_AFTER`] only when the tab could stay scrolled
/// that far in the taller view condensing gives it: otherwise the list
/// clamps its scroll, the header expands, the view shrinks and the two
/// states alternate every frame. Condensed, it stays so until the tab is
/// back at its top.
fn condenses(now: bool, scroll: TabScroll) -> bool {
    match now {
        true => scroll.offset > px(0.),
        false => scroll.offset > CONDENSE_AFTER && scroll.max > CONDENSE_AFTER + CONDENSE_ROOM,
    }
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The header condenses while the shown tab is scrolled; read each
        // frame, it costs a field read per tab.
        let scroll = match self.tab {
            DetailTab::Summary => self.summary.read(cx).scroll(),
            DetailTab::Timeline => self.timeline.read(cx).scroll(),
            DetailTab::Code => self.code.read(cx).scroll(),
        };
        self.condensed = condenses(self.condensed, scroll);
        let condensed = self.condensed;
        self.header
            .update(cx, |header, cx| header.set_condensed(condensed, cx));
        let body = match self.tab {
            DetailTab::Summary => self.summary.clone().into_any_element(),
            DetailTab::Timeline => self.timeline.clone().into_any_element(),
            DetailTab::Code => self.code.clone().into_any_element(),
        };
        let (error, notice) = {
            let data = self.data.read(cx);
            (data.error.clone(), data.notice.clone())
        };
        let slot = self.tab_slot(cx);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(self.header.clone())
            .children(error.map(|error| self.render_error(error, cx)))
            .children(notice.map(|notice| self.render_notice(notice, cx)))
            .child(render_tab_row(self.tab, slot, window, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(
                div()
                    .absolute()
                    .bottom_4()
                    .right_4()
                    .child(self.composer.clone()),
            )
            .children(
                self.stack_confirm
                    .and_then(|kind| self.render_stack_confirm(kind, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::conversation::fixtures::{click, commit};
    use crate::main_screen::right_panel::pull_request::fixtures::{detail, hours_ago, repo};
    use github::model::Activity;
    use github::model::MergeMethod;
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::sync::Arc;

    /// A detail page for #290 as if read; no session service behind it,
    /// so every write fails.
    fn page(cx: &mut TestAppContext) -> (Entity<DetailView>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| DetailView::new(repo().pull_request(290), None, true, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let data = view.read_with(cx, |view, _| view.data().clone());
        data.update(cx, |data, cx| {
            data.detail = Some(Arc::new(detail(290)));
            data.stack = super::super::stack::StackView::known_alone();
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn scrolled(offset: f32, max: f32) -> TabScroll {
        TabScroll {
            offset: px(offset),
            max: px(max),
        }
    }

    #[test]
    fn a_long_tab_condenses_the_header_past_the_fold() {
        assert!(!condenses(false, scrolled(20., 800.)));
        assert!(condenses(false, scrolled(30., 800.)));
    }

    #[test]
    fn a_tab_the_taller_view_would_fit_never_condenses() {
        // Condensing would give the list more room than it can scroll; its
        // clamp would expand the header again, every frame.
        assert!(!condenses(false, scrolled(30., 60.)));
        assert!(!condenses(false, scrolled(60., 150.)));
    }

    #[test]
    fn a_condensed_header_stays_until_the_tab_is_back_at_its_top() {
        assert!(condenses(true, scrolled(5., 40.)));
        assert!(!condenses(true, scrolled(0., 800.)));
    }

    #[gpui_kit::test]
    fn a_commit_clicked_in_the_timeline_opens_the_code_tab_on_it(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        let data = view.read_with(cx, |view, _| view.data().clone());
        data.update(cx, |data, cx| {
            data.activity = Some(Arc::new(Activity {
                author: None,
                reviewers: Vec::new(),
                comments: Vec::new(),
                comment_count: 0,
                comments_truncated: false,
                review_threads: Vec::new(),
                review_threads_truncated: false,
                commits: vec![commit("d9aea40f", hours_ago(2))],
                reactions: Vec::new(),
            }));
            cx.notify();
        });
        view.update(cx, |view, cx| view.select_tab(DetailTab::Timeline, cx));
        cx.run_until_parked();

        click(cx, "pr-commit-d9aea40");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tab, DetailTab::Code);
            assert_eq!(view.code.read(cx).commit.as_deref(), Some("d9aea40f"));
        });
    }

    #[gpui_kit::test]
    fn a_failed_action_shows_and_frees_the_controls(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        assert!(cx.debug_bounds("pr-error").is_none());
        view.update(cx, |view, cx| {
            view.act(Action::Merge(MergeMethod::Merge), cx).detach()
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-error").is_some());
        let header = view.read_with(cx, |view, _| view.header.clone());
        header.read_with(cx, |header, _| assert!(!header.pending()));
    }

    #[gpui_kit::test]
    fn runs_of_a_fork_are_approved_with_a_second_click(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        assert!(cx.debug_bounds("pr-approve-workflows").is_none());
        let data = view.read_with(cx, |view, _| view.data().clone());
        data.update(cx, |data, cx| {
            let mut waiting = detail(290);
            waiting.workflow_approvals = Some(2);
            waiting
                .viewer_permissions
                .actions
                .push(github::model::PullRequestAction::ApproveWorkflows);
            data.detail = Some(Arc::new(waiting));
            cx.notify();
        });
        cx.run_until_parked();
        click(cx, "pr-approve-workflows");
        view.read_with(cx, |view, _| {
            assert!(view.confirm.is_armed(Confirmable::ApproveWorkflows))
        });
        click(cx, "pr-approve-workflows");
        // No session service: refused, shown, and the button is free again.
        assert!(cx.debug_bounds("pr-error").is_some());
        view.read_with(cx, |view, _| assert!(!view.approving));
    }

    #[gpui_kit::test]
    fn a_checkout_says_where_it_landed(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        let done = PullRequestCheckout {
            session_id: "s".into(),
            path: "/w/pr".into(),
            branch: "feature".into(),
            on_head: true,
        };
        view.update(cx, |view, cx| {
            view.checked_out(CheckoutPlace::Worktree, &Ok(done.clone()), cx)
        });
        cx.run_until_parked();
        let notice = view.read_with(cx, |view, cx| view.data.read(cx).notice.clone());
        let notice = notice.expect("a notice");
        assert_eq!(notice.title.as_ref(), "Checked out");
        assert!(!notice.warning);
        assert!(cx.debug_bounds("pr-notice").is_some());

        let behind = PullRequestCheckout {
            on_head: false,
            ..done
        };
        assert!(checkout_notice(CheckoutPlace::Here, &behind).warning);

        view.update(cx, |view, cx| {
            view.checked_out(CheckoutPlace::Here, &Err(anyhow::anyhow!("dirty")), cx)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-error").is_some());
        assert!(cx.debug_bounds("pr-notice").is_none());
    }

    #[gpui_kit::test]
    fn a_stack_merges_through_its_own_controls_after_a_confirmation(cx: &mut TestAppContext) {
        use super::super::stack::StackView;
        use github::model::{PullRequestState, Stack, StackLayer};
        let (view, cx) = page(cx);
        assert!(cx.debug_bounds("pr-primary").is_some());
        let data = view.read_with(cx, |view, _| view.data().clone());
        let layer = |number: u64| StackLayer {
            number,
            title: Some(format!("Layer {number}")),
            is_draft: Some(false),
            head_sha: Some(format!("sha{number}")),
            head_branch: format!("b{number}"),
            state: PullRequestState::Open,
        };
        data.update(cx, |data, cx| {
            data.stack = StackView {
                read: true,
                stack: Some(Arc::new(Stack {
                    id: "1".into(),
                    number: 3,
                    url: String::new(),
                    base: "main".into(),
                    layers: vec![layer(289), layer(290), layer(291)],
                })),
                fresh: true,
                error: None,
            };
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-stack").is_some());
        assert!(cx.debug_bounds("pr-primary").is_none(), "the stack merges");
        click(cx, "pr-merge-stack");
        assert!(cx.debug_bounds("pr-stack-confirm").is_some());
        click(cx, "pr-stack-confirm");
        // No session service: refused, shown; the confirmation is gone.
        assert!(cx.debug_bounds("pr-stack-confirm").is_none());
        assert!(cx.debug_bounds("pr-error").is_some());

        // Without a stack, the single merge is back.
        data.update(cx, |data, cx| {
            data.stack.stack = None;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-primary").is_some());
        assert!(cx.debug_bounds("pr-stack").is_none());
    }

    #[gpui_kit::test]
    fn a_refused_title_is_taken_back(cx: &mut TestAppContext) {
        let (view, cx) = page(cx);
        let header = view.read_with(cx, |view, _| view.header.clone());
        header.update(cx, |header, cx| header.save_title("Renamed".into(), cx));
        cx.run_until_parked();
        // The header's intent reached the view as the panel would pass it.
        view.update(cx, |view, cx| {
            view.update(
                PullRequestUpdate {
                    title: Some("Renamed".into()),
                    ..PullRequestUpdate::default()
                },
                cx,
            )
        });
        cx.run_until_parked();
        header.read_with(cx, |header, _| {
            assert_eq!(header.shown_title().as_ref(), detail(290).title)
        });
    }
}
