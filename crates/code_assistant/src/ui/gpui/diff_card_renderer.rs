//! Diff card renderer for `edit`, `replace_in_file`, and `write_file` tool blocks.
//!
//! Renders file-editing tools as bordered cards with:
//! - Header: file icon + path, red ✕ on error, chevron toggle
//! - Body: unified diff view (edit, replace_in_file) or content preview (write_file)
//!
//! Replaces the old parameter-renderer-based rendering for these tools.

use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::gpui::file_icons;
use crate::ui::gpui::tool_block_renderers::{ToolBlockRenderer, ToolBlockStyle};
use crate::ui::ToolStatus;
use gpui::{
    div, px, Context, Element, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window,
};
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Collapse state
// ---------------------------------------------------------------------------

static COLLAPSED: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

fn collapsed_state() -> &'static Mutex<HashMap<String, bool>> {
    COLLAPSED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_collapsed(tool_id: &str) -> bool {
    collapsed_state()
        .lock()
        .ok()
        .and_then(|m| m.get(tool_id).copied())
        .unwrap_or(false)
}

fn toggle_collapsed(tool_id: &str) {
    if let Ok(mut m) = collapsed_state().lock() {
        let current = m.get(tool_id).copied().unwrap_or(false);
        m.insert(tool_id.to_string(), !current);
    }
}

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
        ]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        let path = extract_path(tool).unwrap_or_default();
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
        theme: &gpui_component::theme::Theme,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        // We need at least one parameter to show anything.
        if tool.parameters.is_empty() {
            return None;
        }

        let path = extract_path(tool).unwrap_or_default();
        let has_error = tool.status == ToolStatus::Error;
        let collapsed = is_collapsed(&tool.id);
        let is_dark = theme.background.l < 0.5;

        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };

        // --- Card container ---
        let mut card = div()
            .w_full()
            .mt_1()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // --- Header ---
        let tool_id_for_click = tool.id.clone();
        let header_text_color = theme.muted_foreground;

        // Icon
        let icon = file_icons::get().get_tool_icon(&tool.name);

        let icon_fallback = match tool.name.as_str() {
            "edit" => "✎",
            "replace_in_file" => "⇄",
            "write_file" => "✎",
            _ => "📄",
        };

        // Chevron
        let chevron_icon = if collapsed {
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
            .flex_grow();

        header_left = header_left.child(file_icons::render_icon_container(
            &icon,
            13.0,
            header_text_color,
            icon_fallback,
        ));

        // File path (or tool name fallback)
        let header_label = if !path.is_empty() {
            abbreviate_path(&path)
        } else {
            tool.name.replace('_', " ")
        };
        header_left = header_left.child(
            div()
                .text_size(px(12.0))
                .text_color(header_text_color)
                .overflow_hidden()
                .child(header_label),
        );

        let mut header_right = div().flex().flex_row().items_center().gap_2();

        // Red ✕ on error
        if has_error {
            header_right = header_right.child(
                gpui::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
            );
        }

        // Chevron
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(20.))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        card = card.child(
            div()
                .id(SharedString::from(format!("diff-header-{}", tool.id)))
                .px_3()
                .py_1p5()
                .bg(header_bg)
                .cursor_pointer()
                .flex()
                .flex_row()
                .justify_between()
                .items_center()
                .on_click(move |_event, window, _cx| {
                    toggle_collapsed(&tool_id_for_click);
                    window.refresh();
                })
                .child(header_left)
                .child(header_right),
        );

        // --- Body (unless collapsed) ---
        if !collapsed {
            let body_bg = if is_dark {
                gpui::hsla(0.0, 0.0, 0.08, 1.0)
            } else {
                gpui::hsla(0.0, 0.0, 0.97, 1.0)
            };

            let body_content = match tool.name.as_str() {
                "edit" => render_edit_body(tool, theme),
                "replace_in_file" => render_replace_body(tool, theme),
                "write_file" => render_write_body(tool, theme),
                _ => None,
            };

            // Error message from output
            let error_element = if has_error {
                tool.output
                    .as_deref()
                    .filter(|o| !o.is_empty())
                    .map(|output| {
                        div()
                            .px_3()
                            .py_1p5()
                            .text_size(px(12.5))
                            .text_color(theme.danger)
                            .child(output.to_string())
                            .into_any_element()
                    })
            } else {
                None
            };

            if body_content.is_some() || error_element.is_some() {
                let mut body = div()
                    .w_full()
                    .py_1()
                    .bg(body_bg)
                    .flex()
                    .flex_col()
                    .text_size(px(12.5))
                    .font_family("Menlo")
                    .font_weight(FontWeight(400.0))
                    .overflow_hidden();

                if let Some(content) = body_content {
                    body = body.child(content);
                }
                if let Some(error) = error_element {
                    body = body.child(error);
                }

                card = card.child(body);
            }
        }

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Per-tool body rendering
// ---------------------------------------------------------------------------

/// Render body for the `edit` tool.
/// Params: old_text, new_text — show unified diff when both available,
/// streaming fallback (individual colored blocks) otherwise.
fn render_edit_body(
    tool: &ToolUseBlock,
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
    let old_text = get_param(tool, "old_text");
    let new_text = get_param(tool, "new_text");

    match (old_text, new_text) {
        (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
            // Both params available — unified diff
            Some(render_unified_diff(old, new, theme))
        }
        (Some(old), None) if !old.is_empty() => {
            // Streaming: only old_text so far
            Some(render_streaming_block(old, true, theme))
        }
        (None, Some(new)) if !new.is_empty() => {
            // Streaming: only new_text so far (unusual but handle it)
            Some(render_streaming_block(new, false, theme))
        }
        _ => None,
    }
}

/// Render body for the `replace_in_file` tool.
/// Param: diff (SEARCH/REPLACE marker format).
fn render_replace_body(
    tool: &ToolUseBlock,
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
            if section.in_search || section.in_replace {
                render_streaming_diff_section(&section, theme)
            } else {
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

/// Render body for the `write_file` tool.
/// Param: content — shown as all-green additions.
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

// ---------------------------------------------------------------------------
// Diff rendering
// ---------------------------------------------------------------------------

/// Render a unified diff between old and new text using the `similar` crate.
fn render_unified_diff(
    old_text: &str,
    new_text: &str,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(old_text, new_text);

    // Group consecutive lines of the same change type.
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

/// Render a single streaming block (old_text or new_text individually before
/// both are available).
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

/// Render a streaming diff section (partial SEARCH/REPLACE — one side still open).
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
// SEARCH/REPLACE parser (from diff_renderer.rs)
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

    // Normalize markers that got concatenated without newlines
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

    // Trailing section (still streaming)
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

fn extract_path(tool: &ToolUseBlock) -> Option<String> {
    get_param(tool, "path").map(|s| s.to_string())
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

/// Convert RGB u8 values to Hsla.
fn rgb_color(r: u8, g: u8, b: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Convert RGBA u8 values to Hsla.
fn rgba_color(r: u8, g: u8, b: u8, a: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
    .into()
}

/// Returns (background, text_color) for deleted rows.
/// Background is a semi-transparent red tint.
fn deleted_row_colors(theme: &gpui_component::theme::Theme) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x80, 0x20, 0x20, 0x60)), // dark red tint
            rgb_color(0xFF, 0xBB, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0xDD, 0x55, 0x55, 0x30)), // light red tint
            rgb_color(0x88, 0x00, 0x00),
        )
    }
}

/// Returns (background, text_color) for added rows.
/// Background is a semi-transparent green tint.
fn added_row_colors(theme: &gpui_component::theme::Theme) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x20, 0x60, 0x20, 0x60)), // dark green tint
            rgb_color(0xBB, 0xFF, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0x33, 0xAA, 0x33, 0x25)), // light green tint
            rgb_color(0x00, 0x66, 0x00),
        )
    }
}

/// Returns (background, text_color) for unchanged rows.
/// No background — just muted text.
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
        assert!(!sections[0].in_search);
        assert!(!sections[0].in_replace);
    }

    #[test]
    fn test_parse_multiple_sections() {
        let diff = "<<<<<<< SEARCH\nfirst old\n=======\nfirst new\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nsecond old\n=======\nsecond new\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].search_content, "first old");
        assert_eq!(sections[0].replace_content, "first new");
        assert_eq!(sections[1].search_content, "second old");
        assert_eq!(sections[1].replace_content, "second new");
    }

    #[test]
    fn test_parse_streaming_partial() {
        // Streaming: SEARCH started but not closed yet
        let diff = "<<<<<<< SEARCH\npartial content";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].search_content, "partial content");
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
            state: crate::ui::gpui::elements::ToolBlockState::Collapsed,
        };

        assert_eq!(extract_path(&tool), Some("src/main.rs".to_string()));
    }
}
