use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, px, rgb, rgba, Element, FontWeight, IntoElement, ParentElement, Styled};
use similar::{ChangeTag, TextDiff};

/// Renderer for diff-style parameters (replace_in_file "diff" and edit tool "old_text"/"new_text")
pub struct DiffParameterRenderer;

impl ParameterRenderer for DiffParameterRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![
            ("replace_in_file".to_string(), "diff".to_string()),
        ]
    }

    fn render(
        &self,
        tool_name: &str,
        param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        // Handle different parameter types
        match (tool_name, param_name) {
            ("replace_in_file", "diff") => {
                // Traditional diff format - parse and render as before
                div()
                    .rounded_md()
                    .bg(if theme.is_dark() {
                        rgba(0x0A0A0AFF) // Dunklerer Hintergrund im Dark Mode
                    } else {
                        rgba(0xEAEAEAFF) // Hellerer Hintergrund im Light Mode
                    })
                    .p_2()
                    .text_size(px(15.))
                    .font_weight(FontWeight(500.0))
                    .child(parse_and_render_diff(param_value, theme))
                    .into_any()
            }
            _ => {
                // Fallback for unknown parameters
                div()
                    .rounded_md()
                    .bg(if theme.is_dark() {
                        rgba(0x0A0A0AFF)
                    } else {
                        rgba(0xEAEAEAFF)
                    })
                    .p_2()
                    .text_size(px(15.))
                    .font_weight(FontWeight(500.0))
                    .child(param_value.to_string())
                    .into_any()
            }
        }
    }

    fn is_full_width(&self, tool_name: &str, param_name: &str) -> bool {
        // All diff-style parameters are full-width
        matches!(
            (tool_name, param_name),
            ("replace_in_file", "diff")
        )
    }
}

/// Helper function to parse and render a diff content
fn parse_and_render_diff(
    diff_text: &str,
    theme: &gpui_component::theme::Theme,
) -> impl IntoElement {
    // Split the diff text into sections based on the markers
    let sections = parse_diff_sections(diff_text);

    div().flex().flex_col().gap_2().children(
        sections
            .into_iter()
            .map(move |section| render_enhanced_diff_section(section, theme)),
    )
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

    // Normalize diff text to ensure markers are on their own lines
    let normalized_diff = diff_text
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

    for line in normalized_diff.lines() {
        if line == "<<<<<<< SEARCH" || line == "<<<<<<< SEARCH_ALL" {
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
        } else if line == ">>>>>>> REPLACE" || line == ">>>>>>> REPLACE_ALL" {
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
fn render_enhanced_diff_section(
    section: DiffSection,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    if section.in_search || section.in_replace {
        // For streaming blocks we use the simple rendering - a function that returns AnyElement directly
        return render_streaming_diff_section(section, theme);
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
                    // Angepasste Farben fürs Theme
                    let (border_color, text_color) = if theme.is_dark() {
                        (rgba(0x444444FF), rgba(0xFFFFFFAA))
                    } else {
                        (rgba(0x777777FF), rgba(0x333333AA))
                    };

                    // Unchanged lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(border_color)
                        .text_color(text_color)
                        .child(content)
                        .into_any()
                }
                LineChangeType::Deleted => {
                    // Angepasste Farben fürs Theme
                    let (border_color, text_color) = if theme.is_dark() {
                        (rgb(0xCC5555), rgb(0xFFBBBB))
                    } else {
                        (rgb(0xCC3333), rgb(0xAA0000))
                    };

                    // Deleted lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(border_color)
                        .text_color(text_color)
                        .child(content)
                        .into_any()
                }
                LineChangeType::Added => {
                    // Angepasste Farben fürs Theme
                    let (border_color, text_color) = if theme.is_dark() {
                        (rgb(0x55CC55), rgb(0xBBFFBB))
                    } else {
                        (rgb(0x33AA33), rgb(0x007700))
                    };

                    // Added lines group
                    div()
                        .px_2()
                        .border_l_2()
                        .border_color(border_color)
                        .text_color(text_color)
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
        let line_content = change.value().trim_end().to_string();

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
fn render_streaming_diff_section(
    section: DiffSection,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    // Angepasste Farben für das jeweilige Theme
    let (deleted_border, deleted_text, added_border, added_text) = if theme.is_dark() {
        (rgb(0xCC5555), rgb(0xFFBBBB), rgb(0x55CC55), rgb(0xBBFFBB))
    } else {
        (rgb(0xCC3333), rgb(0xAA0000), rgb(0x33AA33), rgb(0x007700))
    };

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
                .border_color(deleted_border)
                .text_color(deleted_text)
                .child(section.search_content.clone())
                .into_any(),
            // Replace content with green border
            div()
                .rounded_md()
                .px_2()
                .py_1()
                .border_l_2()
                .border_color(added_border)
                .text_color(added_text)
                .child(section.replace_content.clone())
                .into_any(),
        ])
        .into_any()
}
