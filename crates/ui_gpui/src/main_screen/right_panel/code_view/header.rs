//! A file's header row: fold chevron, status icon (modified, added,
//! deleted, renamed), path, "Changed" when it changed since it was viewed,
//! `+A −D` and the "Viewed" checkbox (shift-click ticks a range).

use super::list::{CodeHost, toggle_fold};
use crate::shared::theme::merged_violet;
use git::ChangeStatus;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{AnyElement, App, Context, Hsla, SharedString, div, prelude::*, px, svg};

pub fn status_icon(status: ChangeStatus, cx: &App) -> (&'static str, Hsla) {
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

/// The header of the shown file `ix`; `sticky` for the copy kept on top.
pub fn render_file_header<V: CodeHost>(
    this: &V,
    ix: usize,
    sticky: bool,
    cx: &mut Context<V>,
) -> AnyElement {
    if ix >= this.shown_count() {
        return div().into_any_element();
    }
    let list = this.list();
    let file = this.shown_file(ix);
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let folded = list.folds.is_folded(&file.path);
    let chevron = if folded {
        "icons/chevron_right.svg"
    } else {
        "icons/chevron_down.svg"
    };
    let (icon, icon_color) = status_icon(file.status, cx);
    let prefix = match sticky {
        true => format!("{}-sticky", list.prefix),
        false => list.prefix.to_owned(),
    };
    let mark = this.viewed(&file.path);
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
                .truncate()
                .text_color(theme.foreground)
                .child(path),
        )
        .on_click(cx.listener(move |this, _, _, cx| toggle_fold(this, ix, cx)));

    let selector = format!("{prefix}-viewed-{ix}");
    let checkbox = div()
        .flex_none()
        .debug_selector({
            let selector = selector.clone();
            move || selector
        })
        .child(
            Checkbox::new(SharedString::from(selector))
                .label("Viewed")
                .checked(mark.viewed)
                .with_size(Size::XSmall)
                .text_color(muted)
                .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                    let extend = window.modifiers().shift;
                    this.set_viewed(ix, *checked, extend, cx);
                })),
        );

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
        .when(mark.stale, |d| {
            d.child(div().flex_none().text_color(theme.warning).child("Changed"))
        })
        .children(this.render_header_stat(ix, cx))
        .child(checkbox)
        .into_any_element()
}
