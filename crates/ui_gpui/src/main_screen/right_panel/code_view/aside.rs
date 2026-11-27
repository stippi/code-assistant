//! The file tree beside the Code tab's list, after T3 Code's
//! `DiffFileTree`: "Files" with the count and a collapse / expand all
//! folders button, then the folders and files (a file's name and icon
//! tinted by its change, the change's letter at the end); a click on a
//! file shows it from its header. The host may add a footer (a pull
//! request's next page).

use super::list::{CodeHost, reveal_file, toggle_all_folders, toggle_folder};
use super::tree::{TreeRow, tree_rows};
use crate::shared::file_icons;
use git::ChangeStatus;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, Context, Hsla, Pixels, SharedString, Window, div, prelude::*, px, relative, rgb,
    uniform_list,
};
use std::rc::Rc;

/// A tree row's height.
const ROW_HEIGHT: Pixels = px(22.);

/// The letter and colour of a change, as `@pierre/trees` draws them.
fn change_mark(status: ChangeStatus, dark: bool) -> (&'static str, Hsla) {
    let (letter, light, dark_color) = match status {
        ChangeStatus::Added | ChangeStatus::Untracked => ("A", 0x16a994, 0x00cab1),
        ChangeStatus::Deleted => ("D", 0xff2e3f, 0xff6762),
        ChangeStatus::Renamed | ChangeStatus::Copied => ("R", 0xd5a910, 0xffd452),
        ChangeStatus::Modified | ChangeStatus::TypeChanged => ("M", 0x1ca1c7, 0x08c0ef),
    };
    (letter, rgb(if dark { dark_color } else { light }).into())
}

/// The file tree beside the list, when it is open.
pub fn render_file_tree<V: CodeHost>(
    this: &mut V,
    _window: &mut Window,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    let count = this.shown_count();
    if !this.list().code.file_tree || count == 0 {
        return None;
    }
    let prefix = this.list().prefix;
    let footer = this.render_tree_footer(cx);
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let paths: Vec<String> = (0..count)
        .map(|ix| this.shown_file(ix).path.clone())
        .collect();
    let rows = {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        Rc::new(tree_rows(&paths, &this.list().tree_collapsed))
    };
    let header = div()
        .flex_none()
        .h(px(32.))
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child("Files"),
        )
        .child(div().text_color(muted).child(count.to_string()))
        .child(div().flex_1())
        .child(
            Button::new(SharedString::from(format!("{prefix}-tree-folders")))
                .ghost()
                .xsmall()
                .icon(Icon::default().path("icons/chevrons_down_up.svg"))
                .tooltip("Collapse or expand all folders")
                .on_click(cx.listener(|this, _, _, cx| toggle_all_folders(this, cx))),
        );
    let list = uniform_list(
        SharedString::from(format!("{prefix}-tree")),
        rows.len(),
        cx.processor(move |this: &mut V, range: std::ops::Range<usize>, _, cx| {
            range
                .filter_map(|ix| rows.get(ix).map(|row| render_tree_row(this, ix, row, cx)))
                .collect()
        }),
    )
    .track_scroll(&this.list().tree_scroll)
    .flex_1();
    Some(
        div()
            .debug_selector(move || format!("{prefix}-tree"))
            .flex_none()
            .w(relative(0.4))
            .max_w(px(320.))
            .min_w(px(192.))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(border)
            .bg(theme.background)
            .child(header)
            .child(list)
            .children(footer)
            .into_any_element(),
    )
}

fn render_tree_row<V: CodeHost>(
    this: &V,
    ix: usize,
    row: &TreeRow,
    cx: &mut Context<V>,
) -> AnyElement {
    let prefix = this.list().prefix;
    let theme = cx.theme();
    let indent = |depth: usize| px(8. + depth as f32 * 12.);
    let line = div()
        .id((SharedString::from(format!("{prefix}-tree-row")), ix))
        .h(ROW_HEIGHT)
        .w_full()
        .flex()
        .items_center()
        .gap_1()
        .pr_2()
        .text_xs()
        .cursor_pointer()
        .hover(|s| s.bg(theme.list_hover));
    let name = |name: &str, color: Hsla| {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(color)
            .child(SharedString::from(name.to_owned()))
    };
    match row {
        TreeRow::Folder {
            path,
            name: folder,
            depth,
            collapsed,
        } => {
            let icon = file_icons::get().get_type_icon(if *collapsed {
                "collapsed_folder"
            } else {
                "expanded_folder"
            });
            let path = path.clone();
            line.pl(indent(*depth))
                .child(
                    div().w(px(10.)).flex_none().child(
                        gpui_kit::svg()
                            .size(px(10.))
                            .path(if *collapsed {
                                "icons/chevron_right.svg"
                            } else {
                                "icons/chevron_down.svg"
                            })
                            .text_color(theme.muted_foreground),
                    ),
                )
                .child(file_icons::render_icon(
                    &icon,
                    14.0,
                    theme.muted_foreground,
                    "·",
                ))
                .child(name(folder, theme.foreground))
                .on_click(cx.listener(move |this, _, _, cx| toggle_folder(this, path.clone(), cx)))
                .into_any_element()
        }
        TreeRow::File {
            file,
            name: file_name,
            depth,
        } => {
            if *file >= this.shown_count() {
                return div().into_any_element();
            }
            let loaded = this.shown_file(*file);
            let (letter, color) = change_mark(loaded.status, theme.is_dark());
            let selected = this.list().tree_selected.as_deref() == Some(loaded.path.as_str());
            let icon = file_icons::get().get_icon_for_filename(file_name);
            let file = *file;
            let selector = format!("{prefix}-tree-file-{file}");
            line.pl(indent(*depth) + px(14.))
                .debug_selector(move || selector)
                .when(selected, |d| d.bg(theme.list_active))
                .child(file_icons::render_icon(&icon, 14.0, color, "·"))
                .child(name(file_name, color))
                .child(div().flex_none().text_color(color).child(letter))
                .on_click(cx.listener(move |this, _, _, cx| reveal_file(this, file, cx)))
                .into_any_element()
        }
    }
}
