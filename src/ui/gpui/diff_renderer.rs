use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, px, rgb, rgba, Element, FontWeight, IntoElement, ParentElement, Styled};
use similar::{ChangeTag, TextDiff};

/// Renderer for the "diff" parameter of the replace_in_file tool
pub struct DiffParameterRenderer;

impl ParameterRenderer for DiffParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![("replace_in_file".to_string(), "diff".to_string())]
    }

    fn render(&self, _tool_name: &str, _param_name: &str, param_value: &str) -> gpui::AnyElement {
        // Container for the diff content - no parameter name shown
        div()
            .rounded_md()
            .bg(rgba(0x0A0A0AFF))
            .p_2()
            .text_size(px(15.))
            .font_weight(FontWeight(500.0))
            .child(parse_and_render_diff(param_value))
            .into_any()
    }

    fn is_full_width(&self, _tool_name: &str, _param_name: &str) -> bool {
        true // Diff parameter is always full-width
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
#[derive(Clone)]
struct DiffSection {
    search_content: String,
    replace_content: String,
    in_search: bool,
    in_replace: bool,
}

/// Enum to represent the type of change in a line
#[derive(Debug, PartialEq, Clone, Copy)]
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
}

/// Render a diff section with enhanced visualization
fn render_enhanced_diff_section(section: DiffSection) -> gpui::AnyElement {
    if section.in_search || section.in_replace {
        // For streaming blocks we use the simple rendering - a function that returns AnyElement directly
        return render_streaming_diff_section(section);
    }

    // Create the refined diff using similar
    let diff_lines = create_diff_lines(&section.search_content, &section.replace_content);

    // Group consecutive lines of the same type
    let grouped_lines = group_consecutive_lines(diff_lines);

    div()
        .flex()
        .flex_col()
        .children(grouped_lines.into_iter().map(|(change_type, lines)| {
            // Join lines with newlines
            let content = lines.join("\n");

            match change_type {
                LineChangeType::Unchanged => {
                    // Unchanged lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(rgba(0x444444FF))
                        .text_color(rgba(0xFFFFFFAA))
                        .child(content)
                        .into_any()
                }
                LineChangeType::Deleted => {
                    // Deleted lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(rgb(0xCC5555))
                        .text_color(rgb(0xFFBBBB))
                        .child(content)
                        .into_any()
                }
                LineChangeType::Added => {
                    // Added lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(rgb(0x55CC55))
                        .text_color(rgb(0xBBFFBB))
                        .child(content)
                        .into_any()
                }
            }
        }))
        .into_any()
}

// Helper function to group consecutive lines of the same type
fn group_consecutive_lines(diff_lines: Vec<DiffLine>) -> Vec<(LineChangeType, Vec<String>)> {
    let mut grouped = Vec::new();
    let mut current_type: Option<LineChangeType> = None;
    let mut current_lines = Vec::new();

    for line in diff_lines {
        if let Some(line_type) = current_type {
            if line_type == line.change_type {
                // Same type, add to current group
                current_lines.push(line.content);
            } else {
                // Different type, finish current group and start a new one
                grouped.push((line_type, std::mem::take(&mut current_lines)));
                current_type = Some(line.change_type);
                current_lines.push(line.content);
            }
        } else {
            // First line
            current_type = Some(line.change_type);
            current_lines.push(line.content);
        }
    }

    // Add the last group if any
    if let Some(line_type) = current_type {
        if !current_lines.is_empty() {
            grouped.push((line_type, current_lines));
        }
    }

    grouped
}

/// Create a list of diff lines with change information
fn create_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();

    // Create line-by-line diff
    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(old_text, new_text);

    // Process all changes directly
    for change in diff.iter_all_changes() {
        let line_content = change.value().trim().to_string();

        match change.tag() {
            ChangeTag::Equal => {
                // Add unchanged line
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Unchanged,
                });
            }
            ChangeTag::Delete => {
                // Add deleted line
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Deleted,
                });
            }
            ChangeTag::Insert => {
                // Add added line
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Added,
                });
            }
        }
    }

    result
}

/// Helper function specifically for rendering streaming diff blocks, returns AnyElement directly
fn render_streaming_diff_section(section: DiffSection) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(vec![
            // Search content with red border
            div()
                .rounded_md()
                .px_2()
                .py_1()
                .border_l_2()
                .border_color(rgb(0xCC5555))
                .text_color(rgb(0xFFBBBB))
                .child(section.search_content.clone())
                .into_any(),
            // Replace content with green border
            div()
                .rounded_md()
                .px_2()
                .py_1()
                .border_l_2()
                .border_color(rgb(0x55CC55))
                .text_color(rgb(0xBBFFBB))
                .child(section.replace_content.clone())
                .into_any(),
        ])
        .into_any()
}
