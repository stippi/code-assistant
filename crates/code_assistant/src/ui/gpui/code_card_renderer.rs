//! Custom inline renderer for `read_files` and `search_files` tool blocks.
//!
//! Renders file content and search results with line-number gutters,
//! providing a consistent code-viewing experience in the UI.

use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::gpui::tool_block_renderers::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::ui::ToolStatus;
use gpui::{
    div, px, rems, AnyElement, Context, Element, FontWeight, HighlightStyle, ParentElement,
    SharedString, Styled, StyledText, Window,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// CodeCardRenderer
// ---------------------------------------------------------------------------

pub struct CodeCardRenderer;

impl ToolBlockRenderer for CodeCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["read_files".to_string(), "search_files".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        match tool.name.as_str() {
            "read_files" => {
                if let Some(paths) = get_param(tool, "paths") {
                    let display = if paths.len() > 80 {
                        format!("{}…", &paths[..77])
                    } else {
                        paths.to_string()
                    };
                    format!("Read {}", display)
                } else {
                    "Read files".to_string()
                }
            }
            "search_files" => {
                if let Some(regex) = get_param(tool, "regex") {
                    let display = if regex.len() > 60 {
                        format!("{}…", &regex[..57])
                    } else {
                        regex.to_string()
                    };
                    // Trim surrounding quotes from the display value to avoid
                    // visual duplication with the wrapping quotes we add
                    // (e.g. regex `cursor_"` would otherwise show as
                    // `Search for "cursor_""`)
                    let trimmed = display.strip_suffix('"').unwrap_or(&display);
                    let trimmed = trimmed.strip_prefix('"').unwrap_or(trimmed);
                    format!("Search for \"{}\"", trimmed)
                } else {
                    "Search files".to_string()
                }
            }
            _ => tool.name.replace('_', " "),
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        let output = tool.output.as_deref().unwrap_or("");
        if output.is_empty() {
            return None;
        }

        let rem_size = window.rem_size();

        // Try to parse structured JSON output from render_for_ui()
        if let Ok(json) = serde_json::from_str::<Value>(output) {
            match json.get("kind").and_then(|k| k.as_str()) {
                Some("read_files") => {
                    return render_read_files_output(&json, theme, rem_size);
                }
                Some("search_files") => {
                    return render_search_files_output(&json, theme, rem_size);
                }
                _ => {}
            }
        }

        // Fallback: render as plain text (for old sessions or errors)
        render_plain_output(output, tool.status == ToolStatus::Error, theme)
    }
}

// ---------------------------------------------------------------------------
// read_files renderer
// ---------------------------------------------------------------------------

fn render_read_files_output(
    json: &Value,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<AnyElement> {
    let files = json.get("files").and_then(|f| f.as_array())?;
    let errors = json
        .get("errors")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();

    if files.is_empty() && errors.is_empty() {
        return None;
    }

    let line_height_px = rems(1.25).to_pixels(rem_size).round();
    let mut children: Vec<AnyElement> = Vec::new();

    // Render errors first
    for err in &errors {
        let path = err
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("unknown");
        let error = err
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        children.push(
            div()
                .w_full()
                .px_3()
                .py_0p5()
                .text_color(theme.danger)
                .child(format!("✗ {}: {}", path, error))
                .into_any(),
        );
    }

    // Render each file with line numbers
    for file in files {
        let path = file
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("unknown");
        let content = file.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let start_line = file.get("start_line").and_then(|s| s.as_u64()).unwrap_or(1) as usize;

        if content.is_empty() {
            continue;
        }

        // File header
        children.push(
            div()
                .w_full()
                .px_3()
                .py_0p5()
                .text_color(theme.muted_foreground.opacity(0.7))
                .child(format!("── {} ──", path))
                .into_any(),
        );

        // File content with line numbers
        children.push(render_lines_with_gutter(
            content,
            start_line,
            theme.foreground,
            rem_size,
        ));
    }

    Some(
        div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .text_size(rems(0.78125))
            .line_height(line_height_px)
            .font_family("Menlo")
            .font_weight(FontWeight(400.0))
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap_1()
            .children(children)
            .into_any(),
    )
}

// ---------------------------------------------------------------------------
// search_files renderer
// ---------------------------------------------------------------------------

fn render_search_files_output(
    json: &Value,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<AnyElement> {
    let results = json.get("results").and_then(|r| r.as_array())?;
    let document_results = json
        .get("document_results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let total_matches = json
        .get("total_matches")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let truncated = json
        .get("truncated")
        .and_then(|t| t.as_bool())
        .unwrap_or(false);
    let line_height_px = rems(1.25).to_pixels(rem_size).round();

    if results.is_empty() && document_results.is_empty() {
        let regex = json.get("regex").and_then(|r| r.as_str()).unwrap_or("");
        return Some(
            div()
                .pl(px(8.))
                .ml(px(8.))
                .border_l_2()
                .border_color(theme.border)
                .py(px(4.))
                .text_size(rems(0.8125))
                .text_color(theme.muted_foreground)
                .child(format!("No matches found for '{}'", regex))
                .into_any(),
        );
    }

    let mut children: Vec<AnyElement> = Vec::new();

    // Summary header
    let doc_match_count: u64 = document_results
        .iter()
        .filter_map(|d| d.get("match_count").and_then(|c| c.as_u64()))
        .sum();
    let total_display = total_matches + doc_match_count;
    let header_text = if truncated {
        format!(
            "Found {} matches (showing {})",
            total_display,
            results.len() + document_results.len()
        )
    } else {
        format!("Found {} matches", total_display)
    };
    children.push(
        div()
            .w_full()
            .px_3()
            .pb_1()
            .text_color(theme.muted_foreground.opacity(0.7))
            .child(header_text)
            .into_any(),
    );

    // Render each result with line numbers
    for result in results {
        let file = result
            .get("file")
            .and_then(|f| f.as_str())
            .unwrap_or("unknown");
        let start_line = result
            .get("start_line")
            .and_then(|s| s.as_u64())
            .unwrap_or(1) as usize;
        let lines = result
            .get("lines")
            .and_then(|l| l.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
            .unwrap_or_default();
        let match_lines: Vec<usize> = result
            .get("match_lines")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(|n| n as usize))
                    .collect()
            })
            .unwrap_or_default();
        // match_ranges: per match-line, a list of (start, end) byte ranges within that line
        let match_ranges: Vec<Vec<(usize, usize)>> = result
            .get("match_ranges")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|line_ranges| {
                        line_ranges
                            .as_array()
                            .map(|pairs| {
                                pairs
                                    .iter()
                                    .filter_map(|pair| {
                                        let p = pair.as_array()?;
                                        Some((
                                            p.first()?.as_u64()? as usize,
                                            p.get(1)?.as_u64()? as usize,
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();

        if lines.is_empty() {
            continue;
        }

        // File header with line range
        let end_line = start_line + lines.len() - 1;
        children.push(
            div()
                .w_full()
                .px_3()
                .pt_1()
                .pb_0p5()
                .text_color(theme.muted_foreground.opacity(0.7))
                .child(format!("── {}:{}-{} ──", file, start_line, end_line))
                .into_any(),
        );

        // Content with line numbers and inline match highlighting
        children.push(render_search_lines_with_gutter(
            &lines,
            start_line,
            &match_lines,
            &match_ranges,
            theme,
            rem_size,
        ));
    }

    // Render document matches (PDF, DOCX, etc.) – same layout as text matches
    for doc_result in &document_results {
        let file = doc_result
            .get("file")
            .and_then(|f| f.as_str())
            .unwrap_or("unknown");
        let format_str = doc_result
            .get("format")
            .and_then(|f| f.as_str())
            .unwrap_or("");
        let page = doc_result.get("page").and_then(|p| p.as_u64()).unwrap_or(0);
        let start_line = doc_result
            .get("start_line")
            .and_then(|s| s.as_u64())
            .unwrap_or(1) as usize;
        let lines: Vec<String> = doc_result
            .get("lines")
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .or_else(|| {
                // Backward compat: old sessions stored an "excerpt" string instead of "lines"
                doc_result
                    .get("excerpt")
                    .and_then(|e| e.as_str())
                    .map(|s| s.lines().map(|l| l.to_string()).collect())
            })
            .unwrap_or_default();
        let match_lines: Vec<usize> = doc_result
            .get("match_lines")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(|n| n as usize))
                    .collect()
            })
            .unwrap_or_default();
        let match_ranges: Vec<Vec<(usize, usize)>> = doc_result
            .get("match_ranges")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|line_ranges| {
                        line_ranges
                            .as_array()
                            .map(|pairs| {
                                pairs
                                    .iter()
                                    .filter_map(|pair| {
                                        let p = pair.as_array()?;
                                        Some((
                                            p.first()?.as_u64()? as usize,
                                            p.get(1)?.as_u64()? as usize,
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();

        if lines.is_empty() {
            continue;
        }

        // Document header with file, format, page, and line range
        let end_line = start_line + lines.len() - 1;
        let header = if page > 0 {
            format!(
                "── {} ({}, p.{}):{}-{} ──",
                file, format_str, page, start_line, end_line
            )
        } else {
            format!(
                "── {} ({}):{}-{} ──",
                file, format_str, start_line, end_line
            )
        };
        children.push(
            div()
                .w_full()
                .px_3()
                .pt_1()
                .pb_0p5()
                .text_color(theme.muted_foreground.opacity(0.7))
                .child(header)
                .into_any(),
        );

        // Content with line numbers and inline match highlighting (reuse text search renderer)
        let lines_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        children.push(render_search_lines_with_gutter(
            &lines_refs,
            start_line,
            &match_lines,
            &match_ranges,
            theme,
            rem_size,
        ));
    }

    if truncated {
        children.push(
            div()
                .w_full()
                .px_3()
                .pt_1()
                .text_color(theme.muted_foreground.opacity(0.5))
                .text_size(rems(0.75))
                .child("Use the 'paths' parameter to narrow results.")
                .into_any(),
        );
    }

    Some(
        div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .text_size(rems(0.78125))
            .line_height(line_height_px)
            .font_family("Menlo")
            .font_weight(FontWeight(400.0))
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(children)
            .into_any(),
    )
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Render content lines with a line-number gutter (no match highlighting).
/// Used for `read_files` output.
fn render_lines_with_gutter(
    content: &str,
    start_line: usize,
    text_color: gpui::Hsla,
    rem_size: gpui::Pixels,
) -> AnyElement {
    let lines: Vec<&str> = content.lines().collect();
    let max_line_num = start_line + lines.len().saturating_sub(1);
    let gutter_width = max_line_num.to_string().len();

    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    let gutter_color = text_color.opacity(0.35);

    div()
        .flex()
        .flex_col()
        .children(lines.into_iter().enumerate().map(|(i, line)| {
            let line_num = start_line + i;
            let gutter_text = format!("{:>width$}", line_num, width = gutter_width);

            let row = div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                )
                .child(
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
        .into_any()
}

/// Render search result lines with a line-number gutter and inline match
/// highlighting. Match ranges within each line are rendered with a colored
/// background span.
fn render_search_lines_with_gutter(
    lines: &[&str],
    start_line: usize,
    match_lines: &[usize],
    match_ranges: &[Vec<(usize, usize)>],
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> AnyElement {
    let text_color = theme.foreground;
    let max_line_num = start_line + lines.len().saturating_sub(1);
    let gutter_width = max_line_num.to_string().len();

    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    let gutter_color = text_color.opacity(0.35);

    // Inline highlight color for matched text
    let is_dark = theme.background.l < 0.5;
    let highlight_bg = if is_dark {
        gpui::hsla(35.0 / 360.0, 0.9, 0.35, 0.45)
    } else {
        gpui::hsla(45.0 / 360.0, 1.0, 0.65, 0.45)
    };

    div()
        .flex()
        .flex_col()
        .children(lines.iter().enumerate().map(|(i, line)| {
            let line_num = start_line + i;
            let gutter_text = format!("{:>width$}", line_num, width = gutter_width);

            // Find match ranges for this line (if it's a match line)
            let line_match_idx = match_lines.iter().position(|&ml| ml == i);
            let ranges: &[(usize, usize)] = line_match_idx
                .and_then(|idx| match_ranges.get(idx))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            let row = div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                )
                .child(
                    div()
                        .flex_grow()
                        .overflow_x_hidden()
                        .pl_1()
                        .pr_3()
                        .text_color(text_color)
                        .child(render_line_with_highlights(
                            line,
                            ranges,
                            text_color,
                            highlight_bg,
                        )),
                );

            row.into_any()
        }))
        .into_any()
}

/// Render a single line splitting it into normal and highlighted segments.
/// Render a single line with highlighted match ranges using StyledText.
/// This ensures proper text wrapping even for lines with highlights.
fn render_line_with_highlights(
    line: &str,
    ranges: &[(usize, usize)],
    _text_color: gpui::Hsla,
    highlight_bg: gpui::Hsla,
) -> AnyElement {
    if ranges.is_empty() {
        return div().child(line.to_string()).into_any();
    }

    let highlight_style = HighlightStyle {
        background_color: Some(highlight_bg),
        ..Default::default()
    };

    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = ranges
        .iter()
        .filter(|&&(start, end)| start < end && end <= line.len())
        .map(|&(start, end)| (start..end, highlight_style))
        .collect();

    let styled = StyledText::new(SharedString::from(line.to_string())).with_highlights(highlights);

    div().child(styled).into_any()
}

/// Fallback: render plain text output (for old sessions or parse errors).
fn render_plain_output(
    output: &str,
    is_error: bool,
    theme: &gpui_component::theme::Theme,
) -> Option<AnyElement> {
    let output_color = if is_error {
        theme.danger
    } else {
        theme.muted_foreground
    };

    Some(
        div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .text_size(rems(0.8125))
            .text_color(output_color)
            .overflow_hidden()
            .child(output.to_string())
            .into_any(),
    )
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
