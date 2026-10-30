//! Diff card renderer for `edit`, `replace_in_file`, `write_file`, and
//! `delete_files` tool blocks.
//!
//! Renders file-editing tools as bordered cards with:
//! - Header: file icon + path, red ✕ on error, chevron toggle
//! - Body: unified diff view (edit, replace_in_file), content preview
//!   (write_file), or deleted paths list (delete_files)
//!
//! Replaces the old parameter-renderer-based rendering for these tools.

use super::diff_prepare::SectionLines;
use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle, animated_card_body};
use crate::blocks::{BlockView, ToolUseBlock};
use crate::diff::render::{added_row_colors, deleted_row_colors, diff_body_bg, render_diff_lines};
use crate::diff::rows::{EndCallback, LineCallback, RowSelection};
use crate::diff::syntax::DiffSyntax;
use crate::shared::file_icons;
use code_assistant_core::ui::ToolStatus;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    App, ClickEvent, Context, Element, FocusHandle, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, px, rems,
};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// DiffCardRenderer
// ---------------------------------------------------------------------------

pub struct DiffCardRenderer;

impl ToolBlockRenderer for DiffCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "edit".to_string(),
            "replace_in_file".to_string(),
            "write_file".to_string(),
            "delete_files".to_string(),
        ]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        let path = extract_path_or_paths(tool);
        if path.is_empty() {
            tool.name.replace('_', " ")
        } else {
            path
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_kit::component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui_kit::AnyElement> {
        let card_ctx = card_ctx?;
        let rem_size = window.rem_size();

        // We need at least one parameter to show anything.
        if tool.parameters.is_empty() {
            return None;
        }

        let path_label = extract_path_or_paths(tool);
        let has_error = tool.status == ToolStatus::Error;
        let is_dark = theme.background.l < 0.5;

        let scale = card_ctx.animation_scale;
        let is_collapsed = card_ctx.is_collapsed;

        let header_bg = if is_dark {
            gpui_kit::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui_kit::hsla(0.0, 0.0, 0.93, 1.0)
        };

        // --- Card container ---

        let mut card = div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // --- Header ---
        let header_text_color = theme.muted_foreground;

        let icon = file_icons::get().get_tool_icon(&tool.name);
        let icon_fallback = match tool.name.as_str() {
            "edit" => "✎",
            "replace_in_file" => "⇄",
            "write_file" => "✎",
            "delete_files" => "🗑",
            _ => "📄",
        };

        let chevron_icon = if is_collapsed {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        };

        let mut header_left = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .flex_grow(1.0)
            .child(file_icons::render_icon_container(
                &icon,
                13.0,
                header_text_color,
                icon_fallback,
            ));

        let header_label = if !path_label.is_empty() {
            abbreviate_path(&path_label)
        } else {
            tool.name.replace('_', " ")
        };
        let label = div()
            .id(SharedString::from(format!("tool-path-{}", tool.id)))
            .text_size(rems(0.75))
            .text_color(header_text_color)
            .overflow_hidden()
            .child(header_label);
        // A single, still existing file opens in the Files view.
        let single_path = get_param(tool, "path").filter(|_| tool.name != "delete_files");
        header_left = header_left.child(match single_path {
            Some(path) => crate::shared::open_file::clickable(
                label,
                crate::shared::open_file::OpenFileRequest {
                    path: path.to_owned(),
                    line: None,
                    project: get_param(tool, "project").map(str::to_owned),
                },
            )
            .into_any_element(),
            None => label.into_any_element(),
        });

        let mut header_right = div().flex().flex_row().items_center().gap_1();
        if has_error {
            header_right = header_right.child(
                gpui_kit::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
            );
        }
        // Diff/File toggle button for write_file with original_content
        if card_ctx.diff.has_original {
            let diff_mode = card_ctx.write_file_diff_mode;
            let label: SharedString = if diff_mode { "diff" } else { "file" }.into();
            let btn_text_color = if diff_mode {
                theme.accent
            } else {
                header_text_color
            };
            header_right = header_right.child(
                div()
                    .id(SharedString::from(format!("diff-toggle-{}", tool.id)))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1p5()
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(header_text_color.opacity(0.1)))
                    .text_size(rems(0.6875))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(btn_text_color)
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_write_file_diff_mode(cx);
                    }))
                    .child(label),
            );
        }
        // Copy button — shown only while a selection exists in this card's
        // diff. Copies the selected whole lines (Cmd/Ctrl-C does the same).
        if card_ctx.diff_selection.is_some() {
            header_right = header_right.child(
                div()
                    .id(SharedString::from(format!("diff-copy-{}", tool.id)))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1p5()
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(header_text_color.opacity(0.1)))
                    .text_size(rems(0.6875))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.accent)
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.copy_diff_selection(cx);
                        cx.stop_propagation();
                    }))
                    .child(SharedString::from("copy")),
            );
        }
        // Chevron — highlights on header hover via group
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(6.))
                .group_hover("diff-header", |s| s.bg(header_text_color.opacity(0.1)))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        // Header corners: all rounded when collapsed, only top when expanded.
        let header = div()
            .id(SharedString::from(format!("diff-header-{}", tool.id)))
            .group("diff-header")
            .px_3()
            .py_1p5()
            .bg(header_bg)
            .cursor_pointer()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .map(|d| {
                if scale <= 0.0 {
                    d.rounded(px(4.))
                } else {
                    d.rounded_t(px(4.))
                }
            })
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.toggle_tool_collapsed(cx);
            }))
            .child(header_left)
            .child(header_right);

        card = card.child(header);

        // --- Body (animated) ---
        if scale > 0.0 {
            let body_bg = diff_body_bg(theme);

            // Selectable diff rows: wired to the owning BlockView so a
            // drag-select drives its selection and Cmd/Ctrl-C / the Copy
            // button read from it.
            let selection = DiffCardSelection {
                entity: cx.entity().downgrade(),
                focus: card_ctx.diff_focus_handle.clone(),
                range: card_ctx.diff_selection,
                color: theme.selection,
            };

            let body_content = match (tool.name.as_str(), &card_ctx.diff.sections) {
                ("delete_files", _) => render_delete_body(tool, theme),
                (_, Some(sections)) => Some(render_prepared_diff(
                    sections,
                    card_ctx.diff.syntax.as_deref().map(Vec::as_slice),
                    theme,
                    rem_size,
                    Some(&selection),
                )),
                // Still streaming, or a large diff is being computed.
                ("edit", None) => render_streaming_edit(tool, theme),
                ("replace_in_file", None) => render_streaming_replace(tool, theme),
                ("write_file", None) => render_streaming_write(tool, theme, rem_size),
                _ => None,
            };

            let error_element = if has_error {
                tool.output
                    .as_deref()
                    .filter(|o| !o.is_empty())
                    .map(|output| {
                        div()
                            .px_3()
                            .py_1p5()
                            .text_size(rems(0.78125))
                            .text_color(theme.danger)
                            .child(output.to_string())
                            .into_any_element()
                    })
            } else {
                None
            };

            if body_content.is_some() || error_element.is_some() {
                // Round line height to whole pixels to avoid sub-pixel gaps
                // between adjacent rows with different background colors.
                let line_height_px = rems(1.25).to_pixels(rem_size).round();

                let mut body_inner = div()
                    .w_full()
                    .py_1()
                    .bg(body_bg)
                    .rounded_b(px(4.))
                    .flex()
                    .flex_col()
                    .text_size(rems(0.78125))
                    .line_height(line_height_px)
                    .font_family("Menlo")
                    .font_weight(FontWeight(400.0))
                    .overflow_hidden();

                if let Some(content) = body_content {
                    body_inner = body_inner.child(content);
                }
                if let Some(error) = error_element {
                    body_inner = body_inner.child(error);
                }

                card = card.child(animated_card_body(
                    body_inner,
                    scale,
                    card_ctx.content_height.clone(),
                ));
            }
        }

        // Focus + Cmd/Ctrl-C so a drag-selected diff can be copied from the
        // keyboard (the drag's start focuses the card; see `DiffCardSelection`).
        let card = card
            .track_focus(&card_ctx.diff_focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                let ks = &event.keystroke;
                if ks.modifiers.secondary() && ks.key == "c" {
                    view.copy_diff_selection(cx);
                    cx.stop_propagation();
                }
            }));

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Selectable diff rows (chat-thread diff cards)
// ---------------------------------------------------------------------------

/// Wiring that makes a diff card's rows selectable: the owning `BlockView`
/// (via a weak handle so the row closures never borrow it), its focus handle,
/// the current selection range and the highlight color. Mirrors the Review
/// panel's selection, but indices are flat across the card's sections.
pub(crate) struct DiffCardSelection {
    entity: WeakEntity<BlockView>,
    focus: FocusHandle,
    /// Inclusive flat-line range currently selected, if any.
    range: Option<(usize, usize)>,
    color: Hsla,
}

impl DiffCardSelection {
    /// The [`RowSelection`] for one section whose first row is flat line
    /// `base_line` and which has `row_count` rows.
    fn row_selection(&self, base_line: usize, row_count: usize) -> RowSelection {
        let highlight = self
            .range
            .map(|(lo, hi)| {
                let start = lo.max(base_line);
                let end = hi.min(base_line + row_count.saturating_sub(1));
                if row_count > 0 && start <= end {
                    (start - base_line)..(end - base_line + 1)
                } else {
                    0..0
                }
            })
            .unwrap_or(0..0);

        let entity = self.entity.clone();
        let on_start: LineCallback = {
            let entity = entity.clone();
            let focus = self.focus.clone();
            Rc::new(move |line, window, cx: &mut App| {
                window.focus(&focus, cx);
                if let Some(view) = entity.upgrade() {
                    view.update(cx, |this, cx| this.begin_diff_selection(line, cx));
                }
            })
        };
        let on_drag: LineCallback = {
            let entity = entity.clone();
            Rc::new(move |line, _window, cx| {
                if let Some(view) = entity.upgrade() {
                    view.update(cx, |this, cx| this.extend_diff_selection(line, cx));
                }
            })
        };
        let on_end: EndCallback = {
            Rc::new(move |_window, cx| {
                if let Some(view) = entity.upgrade() {
                    view.update(cx, |this, _cx| this.end_diff_selection());
                }
            })
        };

        RowSelection {
            base_line,
            highlight,
            color: self.color,
            marked: Vec::new(),
            mark_color: self.color,
            anchor: None,
            on_start,
            on_drag,
            on_end,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-tool body rendering
// ---------------------------------------------------------------------------

/// The finished card: every section's cached rows, highlighted once the
/// syntax parse has arrived.
fn render_prepared_diff(
    sections: &[SectionLines],
    syntax: Option<&[DiffSyntax]>,
    theme: &gpui_kit::component::theme::Theme,
    rem_size: gpui_kit::Pixels,
    selection: Option<&DiffCardSelection>,
) -> gpui_kit::AnyElement {
    // Flat line index of each section's first row: selection indices run
    // across the whole card, so each section needs its own base.
    let mut base_line = 0usize;
    let mut children = Vec::with_capacity(sections.len());
    for (ix, section) in sections.iter().enumerate() {
        let row_count = section.lines.len();
        let row_selection = selection.map(|sel| sel.row_selection(base_line, row_count));
        children.push(render_diff_lines(
            &section.lines,
            theme,
            section.start_line,
            rem_size,
            syntax.and_then(|syntax| syntax.get(ix)),
            row_selection,
        ));
        base_line += row_count;
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(children)
        .into_any()
}

/// `edit` while its parameters stream in: raw red/green blocks. The real
/// diff only makes sense (and is only computed) once the tool is complete.
fn render_streaming_edit(
    tool: &ToolUseBlock,
    theme: &gpui_kit::component::theme::Theme,
) -> Option<gpui_kit::AnyElement> {
    let mut children: Vec<gpui_kit::AnyElement> = Vec::new();
    if let Some(old) = get_param(tool, "old_text").filter(|s| !s.is_empty()) {
        children.push(render_streaming_block(old, true, theme));
    }
    if let Some(new) = get_param(tool, "new_text").filter(|s| !s.is_empty()) {
        children.push(render_streaming_block(new, false, theme));
    }
    (!children.is_empty()).then(|| div().flex().flex_col().children(children).into_any())
}

/// `replace_in_file` while streaming: raw search/replace blocks per section.
fn render_streaming_replace(
    tool: &ToolUseBlock,
    theme: &gpui_kit::component::theme::Theme,
) -> Option<gpui_kit::AnyElement> {
    let sections = parse_diff_sections(get_param(tool, "diff")?);
    (!sections.is_empty()).then(|| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                sections
                    .iter()
                    .map(|section| render_streaming_diff_section(section, theme)),
            )
            .into_any()
    })
}

/// `write_file` while streaming: the content so far as numbered additions.
fn render_streaming_write(
    tool: &ToolUseBlock,
    theme: &gpui_kit::component::theme::Theme,
    rem_size: gpui_kit::Pixels,
) -> Option<gpui_kit::AnyElement> {
    let content = get_param(tool, "content")?;
    if content.is_empty() {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let gutter_width = total_lines.to_string().len();

    // Gutter width in pixels (~0.5rem per digit + 0.75rem padding)
    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    let (row_bg, text_color) = added_row_colors(theme);
    let gutter_color = text_color.opacity(0.5);

    Some(
        div()
            .flex()
            .flex_col()
            .children(lines.into_iter().enumerate().map(|(i, line)| {
                let line_num = i + 1;
                let gutter_text = format!("{:>width$}", line_num, width = gutter_width);

                let mut row = div().w_full().flex().flex_row().items_start();
                if let Some(bg) = row_bg {
                    row = row.bg(bg);
                }

                // Gutter
                row = row.child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                );

                // Content
                row = row.child(
                    div()
                        .flex_grow(1.0)
                        .overflow_x_hidden()
                        .pl_1()
                        .pr_3()
                        .text_color(text_color)
                        .child(line.to_string()),
                );

                row.into_any()
            }))
            .into_any(),
    )
}

/// Render body for the `delete_files` tool — all-red deletions showing paths.
fn render_delete_body(
    tool: &ToolUseBlock,
    theme: &gpui_kit::component::theme::Theme,
) -> Option<gpui_kit::AnyElement> {
    let paths_raw = get_param(tool, "paths")?;
    if paths_raw.is_empty() {
        return None;
    }

    // The paths parameter is a JSON array of strings.
    let paths: Vec<String> =
        serde_json::from_str(paths_raw).unwrap_or_else(|_| vec![paths_raw.to_string()]);

    if paths.is_empty() {
        return None;
    }

    let (row_bg, text_color) = deleted_row_colors(theme);

    Some(
        div()
            .flex()
            .flex_col()
            .children(paths.into_iter().map(|path| {
                let mut row = div().w_full().px_3().text_color(text_color).child(path);
                if let Some(bg) = row_bg {
                    row = row.bg(bg);
                }
                row.into_any()
            }))
            .into_any(),
    )
}

// ---------------------------------------------------------------------------
// Streaming blocks
// ---------------------------------------------------------------------------

fn render_streaming_block(
    text: &str,
    is_deletion: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::AnyElement {
    let (row_bg, text_color) = if is_deletion {
        deleted_row_colors(theme)
    } else {
        added_row_colors(theme)
    };
    let mut row = div()
        .w_full()
        .px_3()
        .text_color(text_color)
        .child(text.to_string());
    if let Some(bg) = row_bg {
        row = row.bg(bg);
    }
    row.into_any()
}

fn render_streaming_diff_section(
    section: &DiffSection,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::AnyElement {
    let (del_bg, del_text) = deleted_row_colors(theme);
    let (add_bg, add_text) = added_row_colors(theme);
    let mut children: Vec<gpui_kit::AnyElement> = Vec::new();

    if !section.search_content.is_empty() {
        let mut row = div()
            .w_full()
            .px_3()
            .text_color(del_text)
            .child(section.search_content.clone());
        if let Some(bg) = del_bg {
            row = row.bg(bg);
        }
        children.push(row.into_any());
    }
    if !section.replace_content.is_empty() {
        let mut row = div()
            .w_full()
            .px_3()
            .text_color(add_text)
            .child(section.replace_content.clone());
        if let Some(bg) = add_bg {
            row = row.bg(bg);
        }
        children.push(row.into_any());
    }

    div().flex().flex_col().children(children).into_any()
}

// ---------------------------------------------------------------------------
// SEARCH/REPLACE parser
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(super) struct DiffSection {
    pub(super) search_content: String,
    pub(super) replace_content: String,
    in_search: bool,
    in_replace: bool,
}

pub(super) fn parse_diff_sections(diff_text: &str) -> Vec<DiffSection> {
    let mut sections = Vec::new();
    let mut current = DiffSection {
        search_content: String::new(),
        replace_content: String::new(),
        in_search: false,
        in_replace: false,
    };

    let normalized = diff_text
        .replace(
            ">>>>>>> REPLACE<<<<<<< SEARCH",
            ">>>>>>> REPLACE\n<<<<<<< SEARCH",
        )
        .replace(
            ">>>>>>> REPLACE_ALL<<<<<<< SEARCH",
            ">>>>>>> REPLACE_ALL\n<<<<<<< SEARCH",
        )
        .replace(
            ">>>>>>> REPLACE<<<<<<< SEARCH_ALL",
            ">>>>>>> REPLACE\n<<<<<<< SEARCH_ALL",
        )
        .replace(
            ">>>>>>> REPLACE_ALL<<<<<<< SEARCH_ALL",
            ">>>>>>> REPLACE_ALL\n<<<<<<< SEARCH_ALL",
        );

    for line in normalized.lines() {
        if line == "<<<<<<< SEARCH" || line == "<<<<<<< SEARCH_ALL" {
            if !current.search_content.is_empty() || !current.replace_content.is_empty() {
                sections.push(current);
                current = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: true,
                    in_replace: false,
                };
            } else {
                current.in_search = true;
                current.in_replace = false;
            }
        } else if line == "=======" {
            current.in_search = false;
            current.in_replace = true;
        } else if line == ">>>>>>> REPLACE" || line == ">>>>>>> REPLACE_ALL" {
            current.in_search = false;
            current.in_replace = false;
            if !current.search_content.is_empty() || !current.replace_content.is_empty() {
                sections.push(current);
                current = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: false,
                    in_replace: false,
                };
            }
        } else if current.in_search {
            if !current.search_content.is_empty() {
                current.search_content.push('\n');
            }
            current.search_content.push_str(line);
        } else if current.in_replace {
            if !current.replace_content.is_empty() {
                current.replace_content.push('\n');
            }
            current.replace_content.push_str(line);
        }
    }

    if !current.search_content.is_empty() || !current.replace_content.is_empty() {
        sections.push(current);
    }
    sections
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn get_param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

/// Extract match start line numbers from the tool's output JSON.
///
/// After execution, `edit` and `replace_in_file` tools emit their output as
/// JSON containing a `match_start_lines` array via `render_for_ui()`.
/// This function attempts to parse that; returns an empty vec on failure.
pub(super) fn parse_match_start_lines(tool: &ToolUseBlock) -> Vec<usize> {
    tool.output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("match_start_lines").cloned())
        .and_then(|v| serde_json::from_value::<Vec<usize>>(v).ok())
        .unwrap_or_default()
}

/// Extract path (single) or paths (array) for the header label.
fn extract_path_or_paths(tool: &ToolUseBlock) -> String {
    if let Some(path) = get_param(tool, "path") {
        return path.to_string();
    }
    if let Some(paths_raw) = get_param(tool, "paths") {
        if let Ok(paths) = serde_json::from_str::<Vec<String>>(paths_raw) {
            return paths.join(", ");
        }
        return paths_raw.to_string();
    }
    String::new()
}

fn abbreviate_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_section() {
        let diff = "<<<<<<< SEARCH\nold line\n=======\nnew line\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].search_content, "old line");
        assert_eq!(sections[0].replace_content, "new line");
    }

    #[test]
    fn test_parse_multiple_sections() {
        let diff = "<<<<<<< SEARCH\nfirst old\n=======\nfirst new\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nsecond old\n=======\nsecond new\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn test_parse_streaming_partial() {
        let diff = "<<<<<<< SEARCH\npartial content";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].in_search);
    }

    #[test]
    fn test_parse_concatenated_markers() {
        let diff = "<<<<<<< SEARCH\nold\n=======\nnew\n>>>>>>> REPLACE<<<<<<< SEARCH\nold2\n=======\nnew2\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn test_extract_path() {
        use crate::blocks::ParameterBlock;
        let tool = ToolUseBlock {
            name: "edit".to_string(),
            id: "test".to_string(),
            parameters: vec![
                ParameterBlock {
                    name: "path".to_string(),
                    value: "src/main.rs".to_string(),
                },
                ParameterBlock {
                    name: "old_text".to_string(),
                    value: "old".to_string(),
                },
            ],

            status: ToolStatus::Success,
            status_message: None,
            output: None,
            output_deferred: false,
            styled_output: None,
            state: crate::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
            revision: 0,
            wakeup_fired: false,
        };
        assert_eq!(extract_path_or_paths(&tool), "src/main.rs");
    }

    #[test]
    fn test_extract_paths_json() {
        use crate::blocks::ParameterBlock;
        let tool = ToolUseBlock {
            name: "delete_files".to_string(),
            id: "test".to_string(),
            parameters: vec![ParameterBlock {
                name: "paths".to_string(),
                value: r#"["a.rs","b.rs"]"#.to_string(),
            }],
            status: ToolStatus::Success,
            status_message: None,
            output: None,
            output_deferred: false,
            styled_output: None,
            state: crate::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
            revision: 0,
            wakeup_fired: false,
        };
        assert_eq!(extract_path_or_paths(&tool), "a.rs, b.rs");
    }
}
