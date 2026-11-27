//! The Code tab's toolbar: the commit scope ("All commits" or one commit,
//! newest first, ten at a time), "6 files · 0 / 6 viewed", hidden tests,
//! and on the right, as in T3 Code: hide whitespace changes, stacked |
//! split, wrap (folded into one menu when the toolbar is narrow), collapse
//! / expand all, hide tests and the file tree.

use super::CodeTab;
use crate::main_screen::right_panel::code_view::options;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{AnyElement, Context, SharedString, WeakEntity, Window, div, prelude::*, px, svg};

/// Commits the scope menu lists at a time.
pub const COMMIT_PAGE: usize = 10;

impl CodeTab {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.render_scope(cx);
        let theme = cx.theme();
        let (muted, warning_color) = (theme.muted_foreground, theme.warning);
        let more = self.pages.last().is_some_and(|p| p.next_cursor.is_some());
        let warning = |id: &'static str, tooltip: &'static str| {
            div()
                .id(id)
                .flex_none()
                .child(
                    svg()
                        .size(px(13.))
                        .path("icons/triangle_alert.svg")
                        .text_color(warning_color),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
                })
        };
        let truncated = self.pages.iter().any(|p| p.truncated);

        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .children(scope)
            .child(options::render_counts(self, more))
            .when(self.viewed.truncated(), |d| {
                d.child(warning(
                    "pr-code-viewed-truncated",
                    "GitHub reports ticks for only part of the files: the count is short.",
                ))
            })
            .when(truncated, |d| {
                d.child(warning(
                    "pr-code-withheld",
                    "GitHub withheld part of this diff: a binary file, or a change too large to show.",
                ))
            })
            .children(options::render_hidden_tests(self, cx))
            .when(self.commit.is_some(), |d| {
                d.child(
                    div()
                        .id("pr-code-commit-scope-note")
                        .flex_none()
                        .child(
                            svg()
                                .size(px(13.))
                                .path("icons/message_square.svg")
                                .text_color(muted.opacity(0.6)),
                        )
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Comments are anchored to the whole change: switch to All commits to write one.",
                            )
                            .build(window, cx)
                        }),
                )
            });

        let right = options::render_diff_options(self, cx);
        options::render_toolbar(self, left.into_any_element(), right, cx)
    }

    /// "All commits ▾", or the chosen commit's headline; `None` before the
    /// commits are known.
    fn render_scope(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let activity = self.activity.as_ref()?;
        if activity.commits.is_empty() {
            return None;
        }
        let label: SharedString = self
            .commit
            .as_ref()
            .and_then(|oid| activity.commits.iter().find(|c| &c.oid == oid))
            .map_or("All commits".into(), |c| c.message_headline.clone().into());
        let view = cx.entity().downgrade();
        Some(
            Popover::new("pr-code-scope")
                .anchor(gpui_kit::Anchor::TopLeft)
                .trigger(
                    Button::new("pr-code-scope-trigger")
                        .ghost()
                        .xsmall()
                        .max_w(px(256.))
                        .label(label)
                        .dropdown_caret(true),
                )
                .content(move |state, window, cx| scope_menu(&view, state, window, cx))
                .into_any_element(),
        )
    }

    pub(super) fn show_more_commits(&mut self, cx: &mut Context<Self>) {
        self.commits_shown += COMMIT_PAGE;
        cx.notify();
    }
}

/// The scope menu: "All commits", then the commits newest first with their
/// short sha, [`COMMIT_PAGE`] at a time.
fn scope_menu(
    view: &WeakEntity<CodeTab>,
    _state: &mut PopoverState,
    _window: &mut Window,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let Some(tab) = view.upgrade() else {
        return div().into_any_element();
    };
    let (mut commits, selected, shown) = {
        let tab = tab.read(cx);
        let commits = tab
            .activity
            .as_ref()
            .map(|a| a.commits.clone())
            .unwrap_or_default();
        (commits, tab.commit.clone(), tab.commits_shown)
    };
    commits.sort_by_key(|c| std::cmp::Reverse(c.committed_date));
    let theme = cx.theme();
    let (accent, muted, mono) = (
        theme.accent,
        theme.muted_foreground,
        theme.mono_font_family.clone(),
    );
    let popover = cx.entity().downgrade();
    let item = |id: SharedString, checked: bool| {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .when(checked, |d| d.bg(accent))
            .hover(|d| d.bg(accent))
    };
    let pick = |commit: Option<String>| {
        let (view, popover) = (view.clone(), popover.clone());
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
            popover
                .update(cx, |state, cx| state.dismiss(window, cx))
                .ok();
            let commit = commit.clone();
            view.update(cx, |tab, cx| tab.select_commit(commit, cx))
                .ok();
        }
    };
    let mut menu = div()
        .w(px(360.))
        .max_w(px(448.))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            item("pr-code-scope-all".into(), selected.is_none())
                .child("All commits")
                .on_click(pick(None)),
        );
    for commit in commits.iter().take(shown) {
        let short: String = commit.oid.chars().take(7).collect();
        menu = menu.child(
            item(
                format!("pr-code-scope-{short}").into(),
                selected.as_ref() == Some(&commit.oid),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(commit.message_headline.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_xs()
                    .text_color(muted)
                    .child(short),
            )
            .on_click(pick(Some(commit.oid.clone()))),
        );
    }
    if commits.len() > shown {
        let view = view.clone();
        menu = menu.child(
            item("pr-code-scope-more".into(), false)
                .text_color(muted)
                .child(format!("Show more ({} left)", commits.len() - shown))
                .on_click(cx.listener(move |_, _, _, cx| {
                    view.update(cx, |tab, cx| tab.show_more_commits(cx)).ok();
                    cx.notify();
                })),
        );
    }
    menu.into_any_element()
}
