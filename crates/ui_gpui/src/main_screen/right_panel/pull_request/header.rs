//! The detail's header: the repository and number (back to the list, open
//! on GitHub), the actions, the title (a click edits it: Enter saves,
//! Escape cancels), author and last update, the checkout command, the
//! branches and the size of the change, and a warning when the branch fell
//! behind its base.
//!
//! Until the detail is read, a listing row (when the pull request was
//! opened from the list) fills in what it knows; the actions wait for the
//! detail.
//!
//! Once the tab below is scrolled, the header condenses: `#N Title` takes
//! the repository's place and one line sums up author, time, branches and
//! the change.

use super::PullRequestIntent;
use super::actions::{
    self, Confirmable, InlineConfirm, allowed_merge_methods, branch_update, checkout_command,
    resolve_merge_method,
};
use super::avatar::avatar;
use super::preferences;
use super::presentation::{Tone, diff_stat, format_count, relative_time};
use super::stack::{self, StackRequest, StackView};
use chrono::{DateTime, Utc};
use github::PullRequestRef;
use github::model::{
    Action, Actor, ChecksState, Detail, ListEntry, MergeMethod, PullRequestUpdate, UpdateMethod,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, Render, SharedString, Subscription,
    Task, Window, div, prelude::*, px, svg,
};
use std::sync::Arc;
use std::time::Duration;

/// How long an armed control waits for its confirming click.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);

struct TitleEditor {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// What the header shows, from the detail or a listing row.
struct Facts {
    number: u64,
    title: SharedString,
    url: String,
    author: Option<Actor>,
    tone: Tone,
    base: SharedString,
    head: SharedString,
    additions: u64,
    deletions: u64,
    changed_files: Option<u64>,
    updated_at: Option<DateTime<Utc>>,
}

pub struct DetailHeader {
    pr: PullRequestRef,
    detail: Option<Arc<Detail>>,
    checks_state: Option<ChecksState>,
    /// A newer detail is being read.
    refreshing: bool,
    /// An action runs.
    pending: bool,
    /// A checkout runs.
    checking_out: bool,
    /// The stack the pull request is a layer of.
    stack: StackView,
    seed: Option<ListEntry>,
    /// The list is there to go back to.
    title_editor: Option<TitleEditor>,
    /// A saved title, shown until the detail has it.
    saved_title: Option<String>,
    /// The merge method the viewer chose in the ⋯ menu.
    picked_merge_method: Option<MergeMethod>,
    confirm: InlineConfirm,
    _confirm_timeout: Option<Task<()>>,
    /// The tab below is scrolled.
    condensed: bool,
    /// The copyable text just copied, ticked for [`COPIED_FOR`].
    copied: Option<&'static str>,
    _copied_reset: Option<Task<()>>,
}

/// How long a copied text shows its tick.
const COPIED_FOR: Duration = Duration::from_millis(1500);

impl EventEmitter<PullRequestIntent> for DetailHeader {}
impl EventEmitter<StackRequest> for DetailHeader {}

impl DetailHeader {
    /// `merge_method` is the one last used in the repository.
    pub fn new(
        pr: PullRequestRef,
        seed: Option<ListEntry>,
        merge_method: Option<MergeMethod>,
    ) -> Self {
        Self {
            pr,
            detail: None,
            checks_state: None,
            refreshing: false,
            pending: false,
            checking_out: false,
            stack: StackView::default(),
            seed,
            title_editor: None,
            saved_title: None,
            picked_merge_method: merge_method,
            confirm: InlineConfirm::default(),
            _confirm_timeout: None,
            condensed: false,
            copied: None,
            _copied_reset: None,
        }
    }

    pub fn set_condensed(&mut self, condensed: bool, cx: &mut Context<Self>) {
        if self.condensed != condensed {
            self.condensed = condensed;
            cx.notify();
        }
    }

    /// What the page read: the detail, the checks' verdict, and whether a
    /// newer state is being read.
    pub fn set_data(
        &mut self,
        detail: Option<Arc<Detail>>,
        checks_state: Option<ChecksState>,
        refreshing: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(detail) = &detail
            && self.saved_title.as_deref() == Some(detail.title.as_str())
        {
            self.saved_title = None;
        }
        self.detail = detail;
        self.checks_state = checks_state;
        self.refreshing = refreshing;
        cx.notify();
    }

    pub fn checks_state(&self) -> Option<ChecksState> {
        self.checks_state
    }

    pub fn set_stack(&mut self, stack: StackView, cx: &mut Context<Self>) {
        if self.stack != stack {
            self.stack = stack;
            cx.notify();
        }
    }

    pub fn stack(&self) -> &StackView {
        &self.stack
    }

    pub fn request_stack(&mut self, request: StackRequest, cx: &mut Context<Self>) {
        cx.emit(request);
    }

    #[cfg(test)]
    pub fn shown_title(&self) -> SharedString {
        self.facts().title
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    pub fn set_checking_out(&mut self, checking_out: bool, cx: &mut Context<Self>) {
        self.checking_out = checking_out;
        cx.notify();
    }

    /// While an action runs, the primary control waits.
    pub fn set_pending(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.pending = pending;
        cx.notify();
    }

    /// The saved title did not make it: show the detail's again.
    pub fn title_rejected(&mut self, cx: &mut Context<Self>) {
        if self.saved_title.take().is_some() {
            cx.notify();
        }
    }

    pub fn emit(&mut self, intent: PullRequestIntent, cx: &mut Context<Self>) {
        cx.emit(intent);
    }

    pub fn confirm(&self) -> InlineConfirm {
        self.confirm
    }

    pub fn merge_method(&self, detail: &Detail) -> MergeMethod {
        resolve_merge_method(&allowed_merge_methods(detail), self.picked_merge_method)
    }

    /// The method is remembered for the repository.
    pub fn pick_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        self.picked_merge_method = Some(method);
        let repo = self.pr.repo.to_string();
        preferences::update(cx, |preferences| {
            preferences.merge_methods.insert(repo, method);
        });
        cx.notify();
    }

    /// A click on a control taking two: the first arms it for a while, the
    /// second acts.
    pub fn click_confirmable(&mut self, key: Confirmable, action: Action, cx: &mut Context<Self>) {
        if self.confirm.click(key) {
            self._confirm_timeout = None;
            cx.emit(PullRequestIntent::Act(action));
        } else {
            self._confirm_timeout = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CONFIRM_TIMEOUT).await;
                this.update(cx, |this, cx| this.disarm(cx)).ok();
            }));
        }
        cx.notify();
    }

    pub fn disarm(&mut self, cx: &mut Context<Self>) {
        self._confirm_timeout = None;
        if self.confirm.disarm() {
            cx.notify();
        }
    }

    fn facts(&self) -> Facts {
        let number = self.pr.number;
        if let Some(d) = &self.detail {
            return Facts {
                number,
                title: self.saved_title.clone().unwrap_or(d.title.clone()).into(),
                url: d.url.clone(),
                author: d.author.clone(),
                tone: Tone::of(d.state, d.is_draft),
                base: d.base_branch.clone().into(),
                head: d.head_branch.clone().into(),
                additions: d.additions,
                deletions: d.deletions,
                changed_files: Some(d.changed_files),
                updated_at: Some(d.updated_at),
            };
        }
        match &self.seed {
            Some(e) => Facts {
                number,
                title: e.title.clone().into(),
                url: e.url.clone(),
                author: e.author.clone(),
                tone: Tone::of(e.state, e.is_draft),
                base: e.base_branch.clone().into(),
                head: e.head_branch.clone().into(),
                additions: e.additions,
                deletions: e.deletions,
                changed_files: None,
                updated_at: Some(e.updated_at),
            },
            None => Facts {
                number,
                title: SharedString::default(),
                url: format!(
                    "https://{}/{}/pull/{number}",
                    self.pr.repo.host,
                    self.pr.repo.full_name()
                ),
                author: None,
                tone: Tone::Open,
                base: SharedString::default(),
                head: SharedString::default(),
                additions: 0,
                deletions: 0,
                changed_files: None,
                updated_at: None,
            },
        }
    }

    fn start_editing(&mut self, title: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, input, event, _, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let title = input.read(cx).value().trim().to_owned();
                    this.save_title(title, cx);
                }
                InputEvent::Blur => this.stop_editing(cx),
                _ => {}
            });
        self.title_editor = Some(TitleEditor {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn save_title(&mut self, title: String, cx: &mut Context<Self>) {
        let unchanged = self.facts().title.as_ref() == title;
        if !title.is_empty() && !unchanged {
            self.saved_title = Some(title.clone());
            cx.emit(PullRequestIntent::Update(PullRequestUpdate {
                title: Some(title),
                ..PullRequestUpdate::default()
            }));
        }
        self.stop_editing(cx);
    }

    fn stop_editing(&mut self, cx: &mut Context<Self>) {
        if self.title_editor.take().is_some() {
            cx.notify();
        }
    }

    /// `owner/repo #290 ↗`, the back arrow before them when there is a list.
    fn render_crumbs(&self, facts: &Facts, cx: &mut Context<Self>) -> impl IntoElement {
        let condensed = self.condensed;
        let repo = &self.pr.repo;
        let repo_url = format!("https://{}/{}", repo.host, repo.full_name());
        let tone = facts.tone.color(cx);
        let url = facts.url.clone();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .when(!condensed, |row| {
                row.child(
                    div()
                        .id("pr-repo")
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.underline())
                        .child(repo.full_name())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.emit(PullRequestIntent::OpenUrl(repo_url.clone()), cx)
                        })),
                )
            })
            .child(
                div()
                    .id("pr-number")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .hover(|s| s.underline())
                    .child(format!("#{}", facts.number))
                    .child(
                        svg()
                            .size(px(11.))
                            .path("icons/external_link.svg")
                            .text_color(tone),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.emit(PullRequestIntent::OpenUrl(url.clone()), cx)
                    })),
            )
            .when(condensed, |row| {
                row.child(
                    div()
                        .debug_selector(|| "pr-condensed-title".into())
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(facts.title.clone()),
                )
            })
    }

    /// The condensed header's one line: author, time, branches, change.
    fn render_summary_line(&self, facts: &Facts, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(muted)
            .child(avatar(facts.author.as_ref(), px(14.), cx))
            .child(
                facts
                    .author
                    .as_ref()
                    .map_or("ghost".to_string(), |a| a.login.clone()),
            )
            .when_some(facts.updated_at, |row, updated| {
                row.child(relative_time(updated, Utc::now()))
            })
            .when(!facts.base.is_empty(), |row| {
                row.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(format!("{} ← {}", facts.base, facts.head)),
                )
            })
            .child(div().flex_1())
            .when_some(facts.changed_files, |row, files| {
                row.child(format!(
                    "{} {}",
                    format_count(files),
                    if files == 1 { "file" } else { "files" }
                ))
            })
            .children(diff_stat(facts.additions, facts.deletions, cx))
    }

    fn render_title(&self, facts: &Facts, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editor) = &self.title_editor {
            return div()
                .capture_action(cx.listener(|this, _: &Escape, _, cx| this.stop_editing(cx)))
                .child(Input::new(&editor.input).small())
                .into_any_element();
        }
        let title = facts.title.clone();
        let editable = self.detail.is_some();
        div()
            .id("pr-title")
            .debug_selector(|| "pr-title".into())
            .text_lg()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().foreground)
            .child(title.clone())
            .when(editable, |el| {
                el.cursor_text()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_editing(title.clone(), window, cx)
                    }))
            })
            .into_any_element()
    }

    /// Author · updated, and the checkout command.
    fn render_meta(&self, facts: &Facts, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let command = checkout_command(facts.number);
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_sm()
            .child(avatar(facts.author.as_ref(), px(18.), cx))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().foreground)
                    .child(
                        facts
                            .author
                            .as_ref()
                            .map_or("ghost".to_string(), |a| a.login.clone()),
                    ),
            )
            .when_some(facts.updated_at, |row, updated| {
                row.child(div().text_color(muted.opacity(0.6)).child("·"))
                    .child(
                        div()
                            .text_color(muted)
                            .child(format!("updated {}", relative_time(updated, Utc::now()))),
                    )
            })
            .child(div().flex_1())
            .child(div().flex_none().text_xs().child(self.copyable(
                "pr-checkout-command",
                command,
                "Copy the checkout command",
                cx,
            )))
    }

    /// Mono `text` that copies itself when clicked; a tick confirms it for
    /// a moment.
    fn copyable(
        &self,
        id: &'static str,
        text: String,
        tooltip: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id(id)
            .debug_selector(move || id.into())
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .hover(|s| s.text_color(theme.foreground))
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .child(div().min_w_0().truncate().child(text.clone()))
            .when(self.copied == Some(id), |row| {
                row.child(
                    svg()
                        .debug_selector(move || format!("{id}-copied"))
                        .flex_none()
                        .size(px(12.))
                        .path("icons/check.svg")
                        .text_color(theme.success),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.emit(PullRequestIntent::Copy(text.clone()), cx);
                this.copied = Some(id);
                this._copied_reset = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    this.update(cx, |this, cx| {
                        this.copied = None;
                        cx.notify();
                    })
                    .ok();
                }));
                cx.notify();
            }))
    }

    /// `base ← head`, and the files and lines changed.
    fn render_branches(&self, facts: &Facts, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(muted)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_none()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(format!("{} ←", facts.base)),
                    )
                    .child(self.copyable(
                        "pr-head-branch",
                        facts.head.to_string(),
                        "Copy the branch name",
                        cx,
                    )),
            )
            .child(div().flex_1())
            .when_some(facts.changed_files, |row, files| {
                row.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            svg()
                                .size(px(13.))
                                .path("icons/file_diff.svg")
                                .text_color(muted),
                        )
                        .child(format!(
                            "{} {}",
                            format_count(files),
                            if files == 1 { "file" } else { "files" }
                        )),
                )
            })
            .children(diff_stat(facts.additions, facts.deletions, cx))
    }

    /// "3 commits behind main" with the button that brings the base in.
    fn render_branch_update(&self, detail: &Detail, cx: &mut Context<Self>) -> Option<AnyElement> {
        let update = branch_update(detail)?;
        let warning = cx.theme().warning;
        let behind = match update.behind_by {
            Some(1) => "1 commit".to_string(),
            Some(n) => format!("{} commits", format_count(n)),
            None => "Commits".to_string(),
        };
        let method = update.methods.first().copied();
        Some(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(warning)
                .child(
                    svg()
                        .size(px(13.))
                        .path("icons/triangle_alert.svg")
                        .text_color(warning),
                )
                .child(format!("{behind} behind {}", detail.base_branch))
                .child(div().flex_1())
                .when_some(method, |row, method| {
                    row.child(
                        Button::new("pr-update-branch")
                            .outline()
                            .xsmall()
                            .label(match method {
                                UpdateMethod::Merge => "Update branch",
                                UpdateMethod::Rebase => "Rebase branch",
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.emit(PullRequestIntent::Act(Action::UpdateBranch(method)), cx)
                            })),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for DetailHeader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facts = self.facts();
        let detail = self.detail.clone();
        let weak = cx.entity().downgrade();
        let actions = detail.as_ref().map(|detail| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .children(stack::render_stack_controls(self, detail, cx))
                .child(actions::render_checkout(
                    facts.number,
                    self.checking_out,
                    weak.clone(),
                ))
                .children(actions::render_primary(self, detail, cx))
                .child(actions::render_more_menu(
                    detail,
                    self.refreshing,
                    weak.clone(),
                ))
        });
        div()
            .flex_none()
            .px_4()
            .pt_2()
            .map(|el| if self.condensed { el.pb_2() } else { el.pb_3() })
            .flex()
            .flex_col()
            .gap_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_crumbs(&facts, cx))
                    .children(actions),
            )
            .map(|el| {
                if self.condensed && self.title_editor.is_none() {
                    return el.child(self.render_summary_line(&facts, cx));
                }
                el.child(self.render_title(&facts, cx))
                    .child(self.render_meta(&facts, cx))
                    .when(!facts.base.is_empty(), |el| {
                        el.child(self.render_branches(&facts, cx))
                    })
                    .children(
                        detail
                            .as_ref()
                            .and_then(|detail| self.render_branch_update(detail, cx)),
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{
        detail, detail_with, entry, repo,
    };
    use crate::main_screen::right_panel::pull_request::presentation::effective_checks_state;
    use github::model::{ChecksState, PullRequestState};
    use gpui_kit::{TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A window showing a header for `detail`, and the intents it emits.
    fn header(
        detail: Option<Detail>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DetailHeader>,
        Rc<RefCell<Vec<PullRequestIntent>>>,
        &mut VisualTestContext,
    ) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| DetailHeader::new(repo().pull_request(290), None, None))
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
        if let Some(detail) = detail {
            let checks = effective_checks_state(detail.checks_state, &detail.checks);
            view.update(cx, |view, cx| {
                view.set_data(Some(Arc::new(detail)), checks, false, cx);
                // In no stack.
                view.set_stack(StackView::known_alone(), cx);
            });
        }
        cx.run_until_parked();
        (view, intents, cx)
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
        cx.simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn merging_takes_a_confirming_click(cx: &mut TestAppContext) {
        let (view, intents, cx) = header(Some(detail(290)), cx);
        click(cx, "pr-primary");
        assert!(intents.borrow().is_empty(), "the first click only arms");
        view.read_with(cx, |view, _| {
            assert!(view.confirm().is_armed(Confirmable::Merge))
        });
        click(cx, "pr-primary");
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::Act(Action::Merge(MergeMethod::Merge))]
        );
    }

    #[gpui_kit::test]
    fn an_armed_merge_disarms_after_a_while(cx: &mut TestAppContext) {
        let (view, intents, cx) = header(Some(detail(290)), cx);
        click(cx, "pr-primary");
        cx.executor().advance_clock(CONFIRM_TIMEOUT);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.confirm().is_armed(Confirmable::Merge))
        });
        click(cx, "pr-primary");
        assert!(intents.borrow().is_empty());
    }

    #[gpui_kit::test]
    fn the_merge_uses_the_picked_method(cx: &mut TestAppContext) {
        let pending = detail_with(290, |d| d.checks_state = Some(ChecksState::Pending));
        let (view, intents, cx) = header(Some(pending), cx);
        view.update(cx, |view, cx| {
            view.pick_merge_method(MergeMethod::Squash, cx)
        });
        click(cx, "pr-primary");
        click(cx, "pr-primary");
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::Act(Action::EnableAutoMerge(
                MergeMethod::Squash
            ))]
        );
    }

    #[gpui_kit::test]
    fn a_merged_pull_request_shows_a_badge(cx: &mut TestAppContext) {
        let merged = detail_with(290, |d| d.state = PullRequestState::Merged);
        let (_, intents, cx) = header(Some(merged), cx);
        click(cx, "pr-primary");
        assert!(intents.borrow().is_empty(), "a badge does nothing");
    }

    #[gpui_kit::test]
    fn a_listing_row_fills_in_until_the_detail_arrives(cx: &mut TestAppContext) {
        let (view, _, cx) = header(None, cx);
        view.update(cx, |view, cx| {
            view.seed = Some(entry(290, "From the list", "ann"));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-title").is_some());
        assert!(cx.debug_bounds("pr-primary").is_none(), "no actions yet");
        view.read_with(cx, |view, _| {
            assert_eq!(view.facts().title.as_ref(), "From the list")
        });
    }

    #[gpui_kit::test]
    fn a_scrolled_tab_condenses_the_header(cx: &mut TestAppContext) {
        let (view, _, cx) = header(Some(detail(290)), cx);
        assert!(cx.debug_bounds("pr-title").is_some());
        view.update(cx, |view, cx| view.set_condensed(true, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-title").is_none());
        assert!(cx.debug_bounds("pr-condensed-title").is_some());
        assert!(cx.debug_bounds("pr-primary").is_some(), "the actions stay");
    }

    #[gpui_kit::test]
    fn the_head_branch_copies_its_name_and_ticks_for_a_moment(cx: &mut TestAppContext) {
        let (_, intents, cx) = header(Some(detail(290)), cx);
        assert!(cx.debug_bounds("pr-head-branch-copied").is_none());
        click(cx, "pr-head-branch");
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::Copy("feat/image-file-renderer".into())]
        );
        assert!(cx.debug_bounds("pr-head-branch-copied").is_some());
        cx.executor().advance_clock(COPIED_FOR);
        cx.run_until_parked();
        assert!(cx.debug_bounds("pr-head-branch-copied").is_none());
    }

    #[gpui_kit::test]
    fn a_saved_title_is_sent_and_shown_until_the_detail_has_it(cx: &mut TestAppContext) {
        let (view, intents, cx) = header(Some(detail(290)), cx);
        click(cx, "pr-title");
        view.update(cx, |view, cx| view.save_title("A better title".into(), cx));
        assert_eq!(
            intents.borrow().as_slice(),
            [PullRequestIntent::Update(PullRequestUpdate {
                title: Some("A better title".into()),
                ..PullRequestUpdate::default()
            })]
        );
        view.read_with(cx, |view, _| {
            assert!(view.title_editor.is_none());
            assert_eq!(view.facts().title.as_ref(), "A better title");
        });

        // Refused: the old title is back.
        view.update(cx, |view, cx| view.title_rejected(cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.facts().title, detail(290).title.as_str())
        });
    }
}
