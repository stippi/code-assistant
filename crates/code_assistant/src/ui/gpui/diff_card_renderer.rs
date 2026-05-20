//! Diff card renderer for `edit`, `replace_in_file`, `write_file`, and
//! `delete_files` tool blocks.
//!
//! Renders file-editing tools as bordered cards with:
//! - Header: file icon + path, red ✕ on error, chevron toggle
//! - Body: unified diff view (edit, replace_in_file), content preview
//!   (write_file), or deleted paths list (delete_files)
//!
//! Replaces the old parameter-renderer-based rendering for these tools.

use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::gpui::file_icons;
use crate::ui::gpui::tool_block_renderers::{
    animated_card_body, CardRenderContext, ToolBlockRenderer, ToolBlockStyle,
};
use crate::ui::ToolStatus;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, rems, ClickEvent, Context, Element, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
};
use similar::{ChangeTag, TextDiff};

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
        is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
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
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
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
            .flex_grow()
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
        header_left = header_left.child(
            div()
                .text_size(rems(0.75))
                .text_color(header_text_color)
                .overflow_hidden()
                .child(header_label),
        );

        let mut header_right = div().flex().flex_row().items_center().gap_1();
        if has_error {
            header_right = header_right.child(
                gpui::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
            );
        }
        // Diff/File toggle button for write_file with original_content
        if tool.name == "write_file" && write_file_has_original_content(tool) {
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
            let body_bg = if is_dark {
                gpui::hsla(0.0, 0.0, 0.08, 1.0)
            } else {
                gpui::hsla(0.0, 0.0, 0.97, 1.0)
            };

            let body_content = match tool.name.as_str() {
                "edit" => render_edit_body(tool, is_generating, theme, rem_size),
                "replace_in_file" => render_replace_body(tool, is_generating, theme, rem_size),
                "write_file" => {
                    render_write_body(tool, theme, rem_size, card_ctx.write_file_diff_mode)
                }
                "delete_files" => render_delete_body(tool, theme),
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

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Per-tool body rendering
// ---------------------------------------------------------------------------

/// Render body for the `edit` tool.
///
/// During streaming (`is_generating`), parameters are still being built up so
/// we show raw red/green blocks.  Once the tool is complete we compute a real
/// unified diff so only the actually-changed lines are highlighted — matching
/// what is shown after a session reload.
fn render_edit_body(
    tool: &ToolUseBlock,
    is_generating: bool,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<gpui::AnyElement> {
    let old_text = get_param(tool, "old_text");
    let new_text = get_param(tool, "new_text");

    if is_generating {
        // Streaming: show whatever we have so far as raw blocks
        let mut children: Vec<gpui::AnyElement> = Vec::new();
        if let Some(old) = old_text.filter(|s| !s.is_empty()) {
            children.push(render_streaming_block(old, true, theme));
        }
        if let Some(new) = new_text.filter(|s| !s.is_empty()) {
            children.push(render_streaming_block(new, false, theme));
        }
        if children.is_empty() {
            return None;
        }
        Some(div().flex().flex_col().children(children).into_any())
    } else {
        // Completed: compute a proper unified diff
        let start_lines = parse_match_start_lines(tool);
        let start_line = start_lines.first().copied();
        match (old_text, new_text) {
            (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                Some(render_unified_diff(old, new, theme, start_line, rem_size))
            }
            (Some(old), None) if !old.is_empty() => Some(render_streaming_block(old, true, theme)),
            (None, Some(new)) if !new.is_empty() => Some(render_streaming_block(new, false, theme)),
            _ => None,
        }
    }
}

/// Render body for the `replace_in_file` tool.
///
/// Same streaming/completed split as `render_edit_body`: during streaming we
/// show raw search/replace blocks, after completion we show unified diffs.
fn render_replace_body(
    tool: &ToolUseBlock,
    is_generating: bool,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<gpui::AnyElement> {
    let diff_text = get_param(tool, "diff")?;
    if diff_text.is_empty() {
        return None;
    }

    let sections = parse_diff_sections(diff_text);
    if sections.is_empty() {
        return None;
    }

    let start_lines = parse_match_start_lines(tool);

    let children: Vec<gpui::AnyElement> = sections
        .into_iter()
        .enumerate()
        .map(|(i, section)| {
            if is_generating || section.in_search || section.in_replace {
                // Streaming or incomplete section: show raw blocks
                render_streaming_diff_section(&section, theme)
            } else {
                // Completed section: compute proper unified diff
                let start_line = start_lines.get(i).copied();
                render_unified_diff(
                    &section.search_content,
                    &section.replace_content,
                    theme,
                    start_line,
                    rem_size,
                )
            }
        })
        .collect();

    Some(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(children)
            .into_any(),
    )
}

/// Render body for the `write_file` tool.
///
/// When `diff_mode` is true and the tool output contains `original_content`
/// (indicating an existing file was overwritten), renders a unified diff.
/// Otherwise falls back to all-green additions with line numbers.
fn render_write_body(
    tool: &ToolUseBlock,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
    diff_mode: bool,
) -> Option<gpui::AnyElement> {
    let content = get_param(tool, "content")?;
    if content.is_empty() {
        return None;
    }

    // Try to extract original_content from the tool output JSON
    let original_content = tool
        .output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("original_content")
                .and_then(|c| c.as_str())
                .map(String::from)
        });

    // If we have original content and diff mode is on, show a unified diff
    if diff_mode {
        if let Some(ref original) = original_content {
            return Some(render_unified_diff(
                original,
                content,
                theme,
                Some(1),
                rem_size,
            ));
        }
    }

    // Fall back to all-green additions (new file or diff mode toggled off)
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
                        .flex_grow()
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
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
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
// Diff rendering
// ---------------------------------------------------------------------------

/// Normalize text for diff display.
///
/// LLMs frequently emit a spurious leading `\n` at the start of `old_text` or
/// `new_text` JSON string values, and the two sides are not always consistent.
/// Additionally, format-on-save only updates `new_text` (the replace side) while
/// `old_text` keeps the raw LLM value, which can introduce trailing-newline
/// mismatches.
///
/// We strip one leading `\n` (if present) so both sides start at real content,
/// then ensure both end with exactly one `\n` so `TextDiff` with
/// `newline_terminated(true)` treats the last line consistently.  Interior blank
/// lines (intentional insertions) are preserved.
fn normalize_for_diff(text: &str) -> String {
    let trimmed = text.strip_prefix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
    format!("{trimmed}\n")
}

fn render_unified_diff(
    old_text: &str,
    new_text: &str,
    theme: &gpui_component::theme::Theme,
    start_line: Option<usize>,
    rem_size: gpui::Pixels,
) -> gpui::AnyElement {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    // Collect individual lines with their tags for line-number rendering
    struct DiffLine {
        tag: ChangeTag,
        text: String,
    }
    let mut diff_lines: Vec<DiffLine> = Vec::new();
    for change in diff.iter_all_changes() {
        diff_lines.push(DiffLine {
            tag: change.tag(),
            text: change.value().trim_end().to_string(),
        });
    }

    // Compute the gutter width (number of digits) based on new-file line numbers
    let gutter_width = if let Some(start) = start_line {
        let new_count = diff_lines
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .count();
        let max_line = start + new_count;
        max_line.to_string().len()
    } else {
        0
    };

    // Track both old and new line numbers
    let mut old_line_num = start_line.unwrap_or(1);
    let mut new_line_num = start_line.unwrap_or(1);

    // Gutter width: compute in rems (~0.5rem per digit + 0.75rem padding),
    // then convert to rounded pixels so it aligns to the pixel grid.
    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    div()
        .flex()
        .flex_col()
        .children(diff_lines.into_iter().map(|dl| {
            let (row_bg, text_color) = match dl.tag {
                ChangeTag::Equal => unchanged_row_colors(theme),
                ChangeTag::Delete => deleted_row_colors(theme),
                ChangeTag::Insert => added_row_colors(theme),
            };

            let mut row = div().w_full().flex().flex_row().items_start();
            if let Some(bg) = row_bg {
                row = row.bg(bg);
            }

            // Gutter with line number (shows new-file line numbers)
            if start_line.is_some() {
                let gutter_text = match dl.tag {
                    ChangeTag::Equal => {
                        let num = new_line_num;
                        old_line_num += 1;
                        new_line_num += 1;
                        format!("{:>width$}", num, width = gutter_width)
                    }
                    ChangeTag::Delete => {
                        old_line_num += 1;
                        format!("{:>width$}", "", width = gutter_width)
                    }
                    ChangeTag::Insert => {
                        let num = new_line_num;
                        new_line_num += 1;
                        format!("{:>width$}", num, width = gutter_width)
                    }
                };
                let gutter_color = match dl.tag {
                    ChangeTag::Equal => unchanged_row_colors(theme).1.opacity(0.5),
                    ChangeTag::Delete => deleted_row_colors(theme).1.opacity(0.5),
                    ChangeTag::Insert => added_row_colors(theme).1.opacity(0.5),
                };
                row = row.child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                );
            }

            // Content — overflow_x_hidden enables min-width:0 in flex so text
            // wraps instead of pushing the row wider than the card.
            row = row.child(
                div()
                    .flex_grow()
                    .overflow_x_hidden()
                    .when(start_line.is_none(), |d| d.px_3())
                    .when(start_line.is_some(), |d| d.pl_1().pr_3())
                    .text_color(text_color)
                    .child(dl.text),
            );

            row.into_any()
        }))
        .into_any()
}

fn render_streaming_block(
    text: &str,
    is_deletion: bool,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
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
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    let (del_bg, del_text) = deleted_row_colors(theme);
    let (add_bg, add_text) = added_row_colors(theme);
    let mut children: Vec<gpui::AnyElement> = Vec::new();

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
struct DiffSection {
    search_content: String,
    replace_content: String,
    in_search: bool,
    in_replace: bool,
}

fn parse_diff_sections(diff_text: &str) -> Vec<DiffSection> {
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

fn get_param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

/// Check whether a write_file tool's output JSON contains `original_content`,
/// indicating the file was overwritten (not newly created).
fn write_file_has_original_content(tool: &ToolUseBlock) -> bool {
    tool.output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("original_content").cloned())
        .is_some()
}

/// Extract match start line numbers from the tool's output JSON.
///
/// After execution, `edit` and `replace_in_file` tools emit their output as
/// JSON containing a `match_start_lines` array via `render_for_ui()`.
/// This function attempts to parse that; returns an empty vec on failure.
fn parse_match_start_lines(tool: &ToolUseBlock) -> Vec<usize> {
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

// ---------------------------------------------------------------------------
// Theme colors
// ---------------------------------------------------------------------------

fn rgb_color(r: u8, g: u8, b: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn rgba_color(r: u8, g: u8, b: u8, a: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
    .into()
}

fn deleted_row_colors(theme: &gpui_component::theme::Theme) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x80, 0x20, 0x20, 0x60)),
            rgb_color(0xFF, 0xBB, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0xDD, 0x55, 0x55, 0x30)),
            rgb_color(0x88, 0x00, 0x00),
        )
    }
}

fn added_row_colors(theme: &gpui_component::theme::Theme) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x20, 0x60, 0x20, 0x60)),
            rgb_color(0xBB, 0xFF, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0x33, 0xAA, 0x33, 0x25)),
            rgb_color(0x00, 0x66, 0x00),
        )
    }
}

fn unchanged_row_colors(theme: &gpui_component::theme::Theme) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (None, rgba_color(0xFF, 0xFF, 0xFF, 0x99))
    } else {
        (None, rgba_color(0x33, 0x33, 0x33, 0x99))
    }
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
        use crate::ui::gpui::elements::ParameterBlock;
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
            styled_output: None,
            state: crate::ui::gpui::elements::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        };
        assert_eq!(extract_path_or_paths(&tool), "src/main.rs");
    }

    #[test]
    fn test_extract_paths_json() {
        use crate::ui::gpui::elements::ParameterBlock;
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
            styled_output: None,
            state: crate::ui::gpui::elements::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        };
        assert_eq!(extract_path_or_paths(&tool), "a.rs, b.rs");
    }

    /// Helper: compute diff tags after the same normalization `render_unified_diff` uses.
    fn diff_tags(old: &str, new: &str) -> Vec<(ChangeTag, String)> {
        let old_norm = normalize_for_diff(old);
        let new_norm = normalize_for_diff(new);
        let diff = TextDiff::configure()
            .newline_terminated(true)
            .diff_lines(&old_norm, &new_norm);
        diff.iter_all_changes()
            .map(|c| (c.tag(), c.value().trim_end().to_string()))
            .collect()
    }

    #[test]
    fn test_diff_no_spurious_leading_green_line() {
        // LLMs sometimes emit new_text with a leading \n that old_text lacks.
        // The normalization should prevent a spurious "added empty line" at top.
        let tags = diff_tags(
            "/// comment\n#[derive(Debug)]",
            "\n/// comment\n#[derive(Debug)]\npub new_field: u32,",
        );
        // First line should be Equal, not Insert
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal, got {:?}",
            tags
        );
        assert_eq!(tags[0].1, "/// comment");
        // The actually new line should be Insert
        assert!(tags
            .iter()
            .any(|(t, l)| *t == ChangeTag::Insert && l == "pub new_field: u32,"));
    }

    #[test]
    fn test_diff_trailing_newline_mismatch_no_spurious_change() {
        // old has no trailing \n, new does — should not produce a spurious change
        let tags = diff_tags("line1\nline2", "line1\nline2\n");
        assert!(
            tags.iter().all(|(t, _)| *t == ChangeTag::Equal),
            "trailing newline mismatch should not produce changes: {:?}",
            tags
        );
    }

    #[test]
    fn test_diff_normal_addition() {
        let tags = diff_tags("line1\nline2\n", "line1\nline2\nnew_line\n");
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], (ChangeTag::Equal, "line1".to_string()));
        assert_eq!(tags[1], (ChangeTag::Equal, "line2".to_string()));
        assert_eq!(tags[2], (ChangeTag::Insert, "new_line".to_string()));
    }

    #[test]
    fn test_diff_intentional_blank_line_insertion_preserved() {
        // Adding a blank line between two functions is a real change that must be shown.
        let tags = diff_tags("fn a() {}\nfn b() {}", "fn a() {}\n\nfn b() {}");
        let inserts: Vec<_> = tags
            .iter()
            .filter(|(t, _)| *t == ChangeTag::Insert)
            .collect();
        assert_eq!(inserts.len(), 1, "should insert one blank line: {:?}", tags);
        assert_eq!(inserts[0].1, "", "the inserted line should be blank");
    }

    #[test]
    fn test_diff_leading_newline_mismatch_both_directions() {
        // old has leading \n, new doesn't — should not produce spurious change
        let tags = diff_tags("\nline1\nline2", "line1\nline2\nnew");
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal: {:?}",
            tags
        );
        assert_eq!(tags[0].1, "line1");
    }
}
