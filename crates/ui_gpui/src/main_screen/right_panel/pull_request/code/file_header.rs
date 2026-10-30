//! A file's header row: fold chevron, status icon (modified, added,
//! deleted, renamed), path, "Changed" when it changed since it was viewed,
//! `+A −D` and the "Viewed" checkbox (shift-click ticks a range).

use super::super::presentation::diff_stat;
use super::CodeTab;
use crate::shared::theme::merged_violet;
use git::ChangeStatus;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{AnyElement, App, Context, Hsla, SharedString, div, prelude::*, px, svg};

fn status_icon(status: ChangeStatus, cx: &App) -> (&'static str, Hsla) {
    let theme = cx.theme();
    match status {
        ChangeStatus::Added | ChangeStatus::Untracked => ("icons/circle_plus.svg", theme.success),
        ChangeStatus::Deleted => ("icons/circle_minus.svg", theme.danger),
        ChangeStatus::Renamed | ChangeStatus::Copied => {
            ("icons/circle_arrow_right.svg", merged_violet())
        }
        ChangeStatus::Modified | ChangeStatus::TypeChanged => ("icons/circle_dot.svg", theme.info),
    }
}

impl CodeTab {
    /// The header of the shown file `ix`; `sticky` for the copy kept on top.
    pub(super) fn render_file_header(
        &self,
        ix: usize,
        sticky: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(&shown) = self.shown.get(ix) else {
            return div().into_any_element();
        };
        let file = self.file(shown);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let folded = self.folds.is_folded(&file.path);
        let chevron = if folded {
            "icons/chevron_right.svg"
        } else {
            "icons/chevron_down.svg"
        };
        let (icon, icon_color) = status_icon(file.status, cx);
        let prefix = if sticky { "pr-code-sticky" } else { "pr-code" };
        let viewed = self.viewed.is_viewed(&file.path);
        let stale = self.viewed.is_stale(&file.path);
        let path: SharedString = match &file.orig_path {
            Some(orig) if file.status == ChangeStatus::Renamed => {
                format!("{orig} → {}", file.path).into()
            }
            _ => file.path.clone().into(),
        };

        let toggle = div()
            .id(SharedString::from(format!("{prefix}-file-{ix}")))
            .debug_selector({
                let selector = format!("{prefix}-file-{ix}");
                move || selector
            })
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .child(
                svg()
                    .flex_none()
                    .size(px(12.))
                    .path(chevron)
                    .text_color(muted),
            )
            .child(
                svg()
                    .flex_none()
                    .size(px(14.))
                    .path(icon)
                    .text_color(icon_color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(theme.foreground)
                    .child(path),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(ix, cx)));

        // Drawn here rather than with the component's checkbox, whose box
        // nearly vanishes on dark backgrounds.
        let selector = format!("{prefix}-viewed-{ix}");
        let checkbox = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .child(
                div()
                    .size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .border_1()
                    .map(|d| match viewed {
                        true => d.border_color(theme.primary).bg(theme.primary),
                        false => d.border_color(muted.opacity(0.7)),
                    })
                    .when(viewed, |d| {
                        d.child(
                            svg()
                                .size(px(10.))
                                .path("icons/check.svg")
                                .text_color(theme.primary_foreground),
                        )
                    }),
            )
            .child(div().text_color(muted).child("Viewed"))
            .on_click(cx.listener(move |this, _, window, cx| {
                let extend = window.modifiers().shift;
                this.set_viewed(ix, !viewed, extend, cx);
            }));

        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .text_xs()
            .bg(theme.background)
            .border_t_1()
            .border_b_1()
            .border_color(theme.border)
            .child(toggle)
            .when(stale, |d| {
                d.child(div().flex_none().text_color(theme.warning).child("Changed"))
            })
            .children(diff_stat(file.additions, file.deletions, cx))
            .child(checkbox)
            .into_any_element()
    }
}
