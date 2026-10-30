//! The Code tab's toolbar: the commit scope ("All commits" or one commit,
//! newest first, ten at a time), "6 files · 0 / 6 viewed", hidden tests,
//! and on the right, as in T3 Code: hide whitespace changes, stacked |
//! split, wrap (folded into one menu when the toolbar is narrow), collapse
//! / expand all, hide tests and the file tree.

use super::super::preferences::DiffLayout;
use super::CodeTab;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable};
use gpui_kit::{
    AnyElement, Context, SharedString, WeakEntity, Window, canvas, div, prelude::*, px, svg,
};

/// Commits the scope menu lists at a time.
pub const COMMIT_PAGE: usize = 10;

/// Below this width the diff options fold into a menu (T3 Code: 32rem).
const NARROW: f32 = 512.;

fn icon(path: &'static str) -> Icon {
    Icon::default().path(SharedString::from(path))
}

impl CodeTab {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.render_scope(cx);
        let theme = cx.theme();
        let (muted, warning_color, border) = (theme.muted_foreground, theme.warning, theme.border);
        let files = self.shown.len();
        let more = self.pages.last().is_some_and(|p| p.next_cursor.is_some());
        let viewed = self
            .shown
            .iter()
            .filter(|&&s| self.viewed.is_viewed(&self.file(s).path))
            .count();
        let hidden_tests = if self.code.hide_tests {
            self.test_files
        } else {
            0
        };
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
            .child(
                div()
                    .flex_none()
                    .debug_selector(|| "pr-code-counts".into())
                    .child(format!(
                        "{files} {}{} · {viewed} / {files} viewed",
                        if files == 1 { "file" } else { "files" },
                        if more { "+" } else { "" },
                    )),
            )
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
            .when(hidden_tests > 0, |d| {
                d.child(
                    Button::new("pr-code-hidden-tests")
                        .link()
                        .xsmall()
                        .label(format!(
                            "{hidden_tests} test {} hidden",
                            if hidden_tests == 1 { "file" } else { "files" }
                        ))
                        .on_click(cx.listener(|this, _, _, cx| this.set_hide_tests(false, cx))),
                )
            })
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

        let all_folded = self.all_folded();
        let code = self.code;
        let narrow = self.toolbar_width.get() < px(NARROW);
        let view = cx.entity().downgrade();
        let toggle = |id: &'static str, path: &'static str, on: bool, tip: SharedString| {
            Button::new(id)
                .ghost()
                .xsmall()
                .selected(on)
                .toggled(on)
                .icon(icon(path))
                .tooltip(tip)
        };
        let options = if narrow {
            let view = view.clone();
            vec![
                Button::new("pr-code-options")
                    .ghost()
                    .xsmall()
                    .icon(icon("icons/settings_2.svg"))
                    .tooltip("Diff options")
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                        let act = |change: fn(&mut CodeTab, &mut Context<CodeTab>)| {
                            let view = view.clone();
                            move |_: &gpui_kit::ClickEvent,
                                  _: &mut Window,
                                  cx: &mut gpui_kit::App| {
                                view.update(cx, change).ok();
                            }
                        };
                        menu.item(
                            PopupMenuItem::new("Hide whitespace changes")
                                .checked(code.hide_whitespace)
                                .on_click(act(|tab, cx| {
                                    tab.set_hide_whitespace(!tab.code.hide_whitespace, cx)
                                })),
                        )
                        .item(
                            PopupMenuItem::new("Wrap lines")
                                .checked(code.wrap)
                                .on_click(act(|tab, cx| tab.set_wrap(!tab.code.wrap, cx))),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("Stacked")
                                .checked(code.layout == DiffLayout::Stacked)
                                .on_click(act(|tab, cx| tab.set_layout(DiffLayout::Stacked, cx))),
                        )
                        .item(
                            PopupMenuItem::new("Split")
                                .checked(code.layout == DiffLayout::Split)
                                .on_click(act(|tab, cx| tab.set_layout(DiffLayout::Split, cx))),
                        )
                    })
                    .into_any_element(),
            ]
        } else {
            let hide = code.hide_whitespace;
            let split = code.layout == DiffLayout::Split;
            vec![
                toggle(
                    "pr-code-whitespace",
                    "icons/pilcrow.svg",
                    hide,
                    match hide {
                        true => "Show whitespace changes".into(),
                        false => "Hide whitespace changes".into(),
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_hide_whitespace(!hide, cx)))
                .into_any_element(),
                ButtonGroup::new("pr-code-layout")
                    .ghost()
                    .xsmall()
                    .child(
                        Button::new("pr-code-stacked")
                            .icon(icon("icons/rows_3.svg"))
                            .tooltip("Stacked diff view")
                            .selected(!split),
                    )
                    .child(
                        Button::new("pr-code-split")
                            .icon(icon("icons/columns_2.svg"))
                            .tooltip("Split diff view")
                            .selected(split),
                    )
                    .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                        let layout = match clicked.first() {
                            Some(1) => DiffLayout::Split,
                            _ => DiffLayout::Stacked,
                        };
                        this.set_layout(layout, cx);
                    }))
                    .into_any_element(),
                toggle(
                    "pr-code-wrap",
                    "icons/text_wrap.svg",
                    code.wrap,
                    match code.wrap {
                        true => "Don't wrap lines".into(),
                        false => "Wrap lines".into(),
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_wrap(!code.wrap, cx)))
                .into_any_element(),
            ]
        };
        let right = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .children(options)
            .when(files > 0, |d| {
                d.child(
                    Button::new("pr-code-fold-all")
                        .ghost()
                        .xsmall()
                        .icon(icon(if all_folded {
                            "icons/chevron_up_down.svg"
                        } else {
                            "icons/chevrons_down_up.svg"
                        }))
                        .tooltip(if all_folded {
                            "Expand all files"
                        } else {
                            "Collapse all files"
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.fold_all(!all_folded, cx)),
                        ),
                )
            })
            .when(self.test_files > 0, |d| {
                let hide = code.hide_tests;
                d.child(
                    toggle(
                        "pr-code-hide-tests",
                        "icons/flask_conical_off.svg",
                        hide,
                        match hide {
                            true => "Show test files".into(),
                            false => "Hide test files".into(),
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_hide_tests(!hide, cx))),
                )
            })
            .when(files > 0, |d| {
                let open = code.file_tree;
                d.child(
                    toggle(
                        "pr-code-file-tree",
                        "icons/folder_tree.svg",
                        open,
                        match open {
                            true => "Hide file tree".into(),
                            false => "Show file tree".into(),
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_file_tree(!open, cx))),
                )
            });

        let width = self.toolbar_width.clone();
        let measure = canvas(
            move |bounds, _, _| width.set(bounds.size.width),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        div()
            .relative()
            .flex_none()
            .h(px(40.))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child(measure)
            .child(left)
            .child(right)
            .into_any_element()
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
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
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
