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
        _window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        let card_ctx = card_ctx?;

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

        let mut header_right = div().flex().flex_row().items_center().gap_2();
        if has_error {
            header_right = header_right.child(
                gpui::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
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
                "edit" => render_edit_body(tool, is_generating, theme),
                "replace_in_file" => render_replace_body(tool, is_generating, theme),
                "write_file" => render_write_body(tool, theme),
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
                let mut body_inner = div()
                    .w_full()
                    .py_1()
                    .bg(body_bg)
                    .rounded_b(px(4.))
                    .flex()
                    .flex_col()
                    .text_size(rems(0.78125))
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
        match (old_text, new_text) {
            (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                Some(render_unified_diff(old, new, theme))
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
) -> Option<gpui::AnyElement> {
    let diff_text = get_param(tool, "diff")?;
    if diff_text.is_empty() {
        return None;
    }

    let sections = parse_diff_sections(diff_text);
    if sections.is_empty() {
        return None;
    }

    let children: Vec<gpui::AnyElement> = sections
        .into_iter()
        .map(|section| {
            if is_generating || section.in_search || section.in_replace {
                // Streaming or incomplete section: show raw blocks
                render_streaming_diff_section(&section, theme)
            } else {
                // Completed section: compute proper unified diff
                render_unified_diff(&section.search_content, &section.replace_content, theme)
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

/// Render body for the `write_file` tool — all-green additions.
fn render_write_body(
    tool: &ToolUseBlock,
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
    let content = get_param(tool, "content")?;
    if content.is_empty() {
        return None;
    }

    let (row_bg, text_color) = added_row_colors(theme);
    let mut row = div()
        .w_full()
        .px_3()
        .text_color(text_color)
        .child(content.to_string());
    if let Some(bg) = row_bg {
        row = row.bg(bg);
    }
    Some(row.into_any())
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
) -> gpui::AnyElement {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    let mut groups: Vec<(ChangeTag, Vec<String>)> = Vec::new();
    for change in diff.iter_all_changes() {
        let line = change.value().trim_end().to_string();
        let tag = change.tag();
        if let Some(last) = groups.last_mut() {
            if last.0 == tag {
                last.1.push(line);
                continue;
            }
        }
        groups.push((tag, vec![line]));
    }

    div()
        .flex()
        .flex_col()
        .children(groups.into_iter().map(|(tag, lines)| {
            let content = lines.join("\n");
            let (row_bg, text_color) = match tag {
                ChangeTag::Equal => unchanged_row_colors(theme),
                ChangeTag::Delete => deleted_row_colors(theme),
                ChangeTag::Insert => added_row_colors(theme),
            };
            let mut row = div().w_full().px_3().text_color(text_color).child(content);
            if let Some(bg) = row_bg {
                row = row.bg(bg);
            }
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
