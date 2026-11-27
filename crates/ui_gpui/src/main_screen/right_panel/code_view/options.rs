//! The diff options every code view's toolbar ends in, as in T3 Code:
//! hide whitespace changes, stacked | split, wrap (folded into one menu
//! when the toolbar is narrow), collapse / expand all, hide tests and the
//! file tree; and the "6 files · 0 / 6 viewed" count the toolbar starts
//! with.

use super::list::{
    CodeHost, all_folded, fold_all, set_file_tree, set_hide_tests, set_hide_whitespace, set_layout,
    set_wrap,
};
use super::preferences::DiffLayout;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Selectable, Sizable};
use gpui_kit::{AnyElement, Context, SharedString, Window, canvas, div, prelude::*, px};

/// Below this width the diff options fold into a menu (T3 Code: 32rem).
const NARROW: f32 = 512.;

fn icon(path: &'static str) -> Icon {
    Icon::default().path(SharedString::from(path))
}

/// "6 files · 0 / 6 viewed"; `more` while more files follow.
pub fn render_counts<V: CodeHost>(this: &V, more: bool) -> AnyElement {
    let files = this.shown_count();
    let viewed = (0..files)
        .filter(|&ix| this.viewed(&this.shown_file(ix).path).viewed)
        .count();
    let selector = format!("{}-counts", this.list().prefix);
    div()
        .flex_none()
        .debug_selector(move || selector)
        .child(format!(
            "{files} {}{} · {viewed} / {files} viewed",
            if files == 1 { "file" } else { "files" },
            if more { "+" } else { "" },
        ))
        .into_any_element()
}

/// "2 test files hidden", a link showing them, while tests are hidden.
pub fn render_hidden_tests<V: CodeHost>(this: &V, cx: &mut Context<V>) -> Option<AnyElement> {
    let hidden = match this.list().code.hide_tests {
        true => this.test_files(),
        false => 0,
    };
    (hidden > 0).then(|| {
        Button::new(SharedString::from(format!(
            "{}-hidden-tests",
            this.list().prefix
        )))
        .link()
        .xsmall()
        .label(format!(
            "{hidden} test {} hidden",
            if hidden == 1 { "file" } else { "files" }
        ))
        .on_click(cx.listener(|this, _, _, cx| set_hide_tests(this, false, cx)))
        .into_any_element()
    })
}

/// The diff options at the toolbar's right end.
pub fn render_diff_options<V: CodeHost>(this: &V, cx: &mut Context<V>) -> AnyElement {
    let list = this.list();
    let prefix = list.prefix;
    let files = this.shown_count();
    let all_folded = all_folded(this);
    let code = list.code;
    let narrow = list.toolbar_width.get() < px(NARROW);
    let view = cx.entity().downgrade();
    let toggle = |id: SharedString, path: &'static str, on: bool, tip: SharedString| {
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
            Button::new(SharedString::from(format!("{prefix}-options")))
                .ghost()
                .xsmall()
                .icon(icon("icons/settings_2.svg"))
                .tooltip("Diff options")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                    let act = |change: fn(&mut V, &mut Context<V>)| {
                        let view = view.clone();
                        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                            view.update(cx, change).ok();
                        }
                    };
                    menu.item(
                        PopupMenuItem::new("Hide whitespace changes")
                            .checked(code.hide_whitespace)
                            .on_click(act(|tab, cx| {
                                let hide = !tab.list().code.hide_whitespace;
                                set_hide_whitespace(tab, hide, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new("Wrap lines")
                            .checked(code.wrap)
                            .on_click(act(|tab, cx| {
                                let wrap = !tab.list().code.wrap;
                                set_wrap(tab, wrap, cx)
                            })),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Stacked")
                            .checked(code.layout == DiffLayout::Stacked)
                            .on_click(act(|tab, cx| set_layout(tab, DiffLayout::Stacked, cx))),
                    )
                    .item(
                        PopupMenuItem::new("Split")
                            .checked(code.layout == DiffLayout::Split)
                            .on_click(act(|tab, cx| set_layout(tab, DiffLayout::Split, cx))),
                    )
                })
                .into_any_element(),
        ]
    } else {
        let hide = code.hide_whitespace;
        let split = code.layout == DiffLayout::Split;
        vec![
            toggle(
                SharedString::from(format!("{prefix}-whitespace")),
                "icons/pilcrow.svg",
                hide,
                match hide {
                    true => "Show whitespace changes".into(),
                    false => "Hide whitespace changes".into(),
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| set_hide_whitespace(this, !hide, cx)))
            .into_any_element(),
            ButtonGroup::new(SharedString::from(format!("{prefix}-layout")))
                .ghost()
                .xsmall()
                .child(
                    Button::new(SharedString::from(format!("{prefix}-stacked")))
                        .icon(icon("icons/rows_3.svg"))
                        .tooltip("Stacked diff view")
                        .selected(!split),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}-split")))
                        .icon(icon("icons/columns_2.svg"))
                        .tooltip("Split diff view")
                        .selected(split),
                )
                .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                    let layout = match clicked.first() {
                        Some(1) => DiffLayout::Split,
                        _ => DiffLayout::Stacked,
                    };
                    set_layout(this, layout, cx);
                }))
                .into_any_element(),
            toggle(
                SharedString::from(format!("{prefix}-wrap")),
                "icons/text_wrap.svg",
                code.wrap,
                match code.wrap {
                    true => "Don't wrap lines".into(),
                    false => "Wrap lines".into(),
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| set_wrap(this, !code.wrap, cx)))
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
                Button::new(SharedString::from(format!("{prefix}-fold-all")))
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
                    .on_click(cx.listener(move |this, _, _, cx| fold_all(this, !all_folded, cx))),
            )
        })
        .when(this.test_files() > 0, |d| {
            let hide = code.hide_tests;
            d.child(
                toggle(
                    SharedString::from(format!("{prefix}-hide-tests")),
                    "icons/flask_conical_off.svg",
                    hide,
                    match hide {
                        true => "Show test files".into(),
                        false => "Hide test files".into(),
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| set_hide_tests(this, !hide, cx))),
            )
        })
        .when(files > 0, |d| {
            let open = code.file_tree;
            d.child(
                toggle(
                    SharedString::from(format!("{prefix}-file-tree")),
                    "icons/folder_tree.svg",
                    open,
                    match open {
                        true => "Hide file tree".into(),
                        false => "Show file tree".into(),
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| set_file_tree(this, !open, cx))),
            )
        });

    right.into_any_element()
}

/// A toolbar of `left` and `right` that measures its width (for the
/// narrow options).
pub fn render_toolbar<V: CodeHost>(
    this: &V,
    left: AnyElement,
    right: AnyElement,
    cx: &Context<V>,
) -> AnyElement {
    use gpui_kit::component::ActiveTheme;
    let theme = cx.theme();
    let width = this.list().toolbar_width.clone();
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
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(measure)
        .child(left)
        .child(right)
        .into_any_element()
}
