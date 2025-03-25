use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{
    div, hsla, rgb, rgba, Element, FontWeight, IntoElement, ParentElement, SharedString, Styled,
};
use similar::{ChangeTag, TextDiff};
use std::sync::Arc;

/// Renderer for the "diff" parameter of the replace_in_file tool
pub struct DiffParameterRenderer;

impl ParameterRenderer for DiffParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![("replace_in_file".to_string(), "diff".to_string())]
    }

    fn render(&self, _tool_name: &str, _param_name: &str, param_value: &str) -> gpui::AnyElement {
        // Container for the diff content
        div()
            .rounded_md()
            .bg(rgba(0x0A0A0AFF))
            .p_2()
            .text_sm()
            // Verwende einen generischen Font statt monospace, da FontFamilyId::monospace() nicht verfügbar ist
            .font_weight(FontWeight(500.0))
            .child(parse_and_render_diff(param_value))
            .into_any()
    }
}

/// Helper function to parse and render a diff content
fn parse_and_render_diff(diff_text: &str) -> impl IntoElement {
    // Split the diff text into sections based on the markers
    let sections = parse_diff_sections(diff_text);

    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(sections.into_iter().map(render_enhanced_diff_section))
}

/// Parse the diff text into sections based on the diff markers
fn parse_diff_sections(diff_text: &str) -> Vec<DiffSection> {
    let mut sections = Vec::new();
    let mut current_section = DiffSection {
        search_content: String::new(),
        replace_content: String::new(),
        in_search: false,
        in_replace: false,
    };

    for line in diff_text.lines() {
        if line == "<<<<<<< SEARCH" {
            // Start a new section if we already have content in the current one
            if !current_section.search_content.is_empty()
                || !current_section.replace_content.is_empty()
            {
                sections.push(current_section);
                current_section = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: true,
                    in_replace: false,
                };
            } else {
                current_section.in_search = true;
                current_section.in_replace = false;
            }
        } else if line == "=======" {
            current_section.in_search = false;
            current_section.in_replace = true;
        } else if line == ">>>>>>> REPLACE" {
            current_section.in_search = false;
            current_section.in_replace = false;

            // Only add the section if it has content
            if !current_section.search_content.is_empty()
                || !current_section.replace_content.is_empty()
            {
                sections.push(current_section);
                current_section = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: false,
                    in_replace: false,
                };
            }
        } else if current_section.in_search {
            if !current_section.search_content.is_empty() {
                current_section.search_content.push('\n');
            }
            current_section.search_content.push_str(line);
        } else if current_section.in_replace {
            if !current_section.replace_content.is_empty() {
                current_section.replace_content.push('\n');
            }
            current_section.replace_content.push_str(line);
        }
    }

    // Add the last section if it has content
    if !current_section.search_content.is_empty() || !current_section.replace_content.is_empty() {
        sections.push(current_section);
    }

    sections
}

/// A section of a diff with search and replace content
struct DiffSection {
    search_content: String,
    replace_content: String,
    in_search: bool,
    in_replace: bool,
}

/// Enum to represent the type of change in a line
#[derive(Debug, PartialEq, Clone)]
enum LineChangeType {
    Unchanged,
    Deleted,
    Added,
}

/// Structure to represent a line in the diff with its change type
#[derive(Debug, Clone)]
struct DiffLine {
    content: String,
    change_type: LineChangeType,
    inline_changes: Option<Vec<(usize, usize, LineChangeType)>>, // (start, end, type)
}

/// Render a diff section with enhanced visualization
fn render_enhanced_diff_section(section: DiffSection) -> impl IntoElement {
    // Create the refined diff using similar
    let diff_lines = create_diff_lines(&section.search_content, &section.replace_content);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(diff_lines.into_iter().map(|line| {
            match line.change_type {
                LineChangeType::Unchanged => {
                    // Unchanged line
                    div()
                        .rounded_md()
                        .bg(rgba(0x1A1A1AFF))
                        .px_2()
                        .py_1()
                        .border_l_2()
                        .border_color(rgba(0x444444FF))
                        .text_color(rgba(0xFFFFFFAA))
                        .child(line.content)
                        .into_any()
                }
                LineChangeType::Deleted => {
                    // Deleted line
                    if let Some(inline_changes) = line.inline_changes {
                        // With inline highlighting
                        render_inline_diff_line(line.content, inline_changes, true)
                    } else {
                        // Without inline highlighting
                        div()
                            .rounded_md()
                            .bg(hsla(0., 0.15, 0.2, 0.5))
                            .px_2()
                            .py_1()
                            .border_l_2()
                            .border_color(rgb(0xCC5555))
                            .text_color(rgb(0xFFBBBB))
                            .child(line.content)
                            .into_any()
                    }
                }
                LineChangeType::Added => {
                    // Added line
                    if let Some(inline_changes) = line.inline_changes {
                        // With inline highlighting
                        render_inline_diff_line(line.content, inline_changes, false)
                    } else {
                        // Without inline highlighting
                        div()
                            .rounded_md()
                            .bg(hsla(120., 0.15, 0.2, 0.5))
                            .px_2()
                            .py_1()
                            .border_l_2()
                            .border_color(rgb(0x55CC55))
                            .text_color(rgb(0xBBFFBB))
                            .child(line.content)
                            .into_any()
                    }
                }
            }
        }))
}

/// Create a list of diff lines with change information
fn create_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();

    // Create line-by-line diff
    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(old_text, new_text);

    // Process line changes
    for change in diff.iter_all_changes() {
        let line_content = change.value().to_string();

        match change.tag() {
            ChangeTag::Equal => {
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Unchanged,
                    inline_changes: None,
                });
            }
            ChangeTag::Delete => {
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Deleted,
                    inline_changes: None, // We'll add inline highlighting later
                });
            }
            ChangeTag::Insert => {
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Added,
                    inline_changes: None, // We'll add inline highlighting later
                });
            }
        }
    }

    // Find pairs of deleted and added lines to perform inline diff
    let mut i = 0;
    while i < result.len().saturating_sub(1) {
        if result[i].change_type == LineChangeType::Deleted
            && result[i + 1].change_type == LineChangeType::Added
        {
            // Get word-level changes
            let old_line = &result[i].content;
            let new_line = &result[i + 1].content;

            // Create word-level diff
            let word_diff = TextDiff::configure()
                .timeout(std::time::Duration::from_millis(100))
                .algorithm(similar::Algorithm::Myers)
                .diff_words(old_line, new_line);

            // Process word changes for the deleted line
            let mut old_inline_changes = Vec::new();
            let mut word_index = 0;

            for change in word_diff.iter_all_changes() {
                let word_len = change.value().len();

                if change.tag() != ChangeTag::Equal {
                    old_inline_changes.push((
                        word_index,
                        word_index + word_len,
                        LineChangeType::Deleted,
                    ));
                }

                if change.tag() != ChangeTag::Insert {
                    word_index += word_len;
                }
            }

            // Process word changes for the added line
            let mut new_inline_changes = Vec::new();
            let mut word_index = 0;

            for change in word_diff.iter_all_changes() {
                let word_len = change.value().len();

                if change.tag() != ChangeTag::Equal {
                    new_inline_changes.push((
                        word_index,
                        word_index + word_len,
                        LineChangeType::Added,
                    ));
                }

                if change.tag() != ChangeTag::Delete {
                    word_index += word_len;
                }
            }

            // Apply inline changes
            if !old_inline_changes.is_empty() {
                result[i].inline_changes = Some(old_inline_changes);
            }

            if !new_inline_changes.is_empty() {
                result[i + 1].inline_changes = Some(new_inline_changes);
            }
        }

        i += 1;
    }

    result
}

/// Render a line with inline diff highlighting
fn render_inline_diff_line(
    content: String,
    inline_changes: Vec<(usize, usize, LineChangeType)>,
    is_deletion: bool,
) -> gpui::AnyElement {
    // Base styles for the line container
    let mut line_div = div().rounded_md().px_2().py_1().border_l_2();

    // Apply styles based on line type
    if is_deletion {
        line_div = line_div
            .bg(hsla(0., 0.15, 0.2, 0.5))
            .border_color(rgb(0xCC5555))
            .text_color(rgb(0xFFBBBB));
    } else {
        line_div = line_div
            .bg(hsla(120., 0.15, 0.2, 0.5))
            .border_color(rgb(0x55CC55))
            .text_color(rgb(0xBBFFBB));
    }

    // Create spans for inline highlighting
    let mut spans = Vec::new();
    let mut last_pos = 0;

    // Sort inline changes by start position
    let mut sorted_changes = inline_changes.clone();
    sorted_changes.sort_by_key(|(start, _, _)| *start);

    for (start, end, _) in sorted_changes {
        // Add unchanged text before this change
        if start > last_pos {
            spans.push(
                div()
                    .child(content[last_pos..start].to_string()) /*.inline()*/
                    .into_any(),
            );
        }

        // Add highlighted text
        let highlight_color = if is_deletion {
            rgba(0xFF6666FF) // Brighter red for deleted parts
        } else {
            rgba(0x66FF66FF) // Brighter green for added parts
        };

        spans.push(
            div()
                .child(content[start..end].to_string())
                .bg(if is_deletion {
                    rgba(0xFF666622)
                } else {
                    rgba(0x66FF6622)
                })
                .text_color(highlight_color)
                .font_weight(FontWeight(700.0))
                //.inline()
                .into_any(),
        );

        last_pos = end;
    }

    // Add any remaining text
    if last_pos < content.len() {
        spans.push(
            div()
                .child(content[last_pos..].to_string()) /*.inline()*/
                .into_any(),
        );
    }

    // Return the line with all spans
    line_div.children(spans).into_any()
}

/// Legacy render function - kept for backward compatibility during streaming
fn render_diff_section(section: DiffSection) -> impl IntoElement {
    div().flex().flex_col().gap_1().children(vec![
        // Search content with red background
        div()
            .rounded_md()
            .bg(hsla(0., 0.15, 0.2, 0.5))
            .px_2()
            .py_1()
            .border_l_2()
            .border_color(rgb(0xCC5555))
            .text_color(rgb(0xFFBBBB))
            .child(section.search_content.clone())
            .into_any(),
        // Replace content with green background
        div()
            .rounded_md()
            .bg(hsla(120., 0.15, 0.2, 0.5))
            .px_2()
            .py_1()
            .border_l_2()
            .border_color(rgb(0x55CC55))
            .text_color(rgb(0xBBFFBB))
            .child(section.replace_content.clone())
            .into_any(),
    ])
}
