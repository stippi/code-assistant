use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, hsla, rgb, rgba, Element, FontWeight, IntoElement, ParentElement, Styled};

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
        .children(sections.into_iter().map(render_diff_section))
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

/// Render a diff section with search and replace content
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
            .child(section.search_content)
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
            .child(section.replace_content)
            .into_any(),
    ])
}
