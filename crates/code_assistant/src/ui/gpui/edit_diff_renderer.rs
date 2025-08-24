use crate::ui::gpui::parameter_renderers::{
    ParameterRenderer, VirtualParameterCompletionStrategy, VirtualParameterSpec,
};
use gpui::{div, px, rgb, rgba, Element, FontWeight, ParentElement, Styled};
use std::collections::HashMap;

/// Renderer for edit tool parameters that combines old_text/new_text into a diff view
pub struct EditDiffRenderer;

impl ParameterRenderer for EditDiffRenderer {
    fn supported_parameters(&self) -> Vec<(String, String)> {
        vec![
            ("edit".to_string(), "old_text".to_string()),
            ("edit".to_string(), "new_text".to_string()),
        ]
    }

    fn virtual_parameters(&self) -> Vec<VirtualParameterSpec> {
        vec![VirtualParameterSpec {
            virtual_name: "diff".to_string(),
            source_params: vec!["old_text".to_string(), "new_text".to_string()],
            completion_strategy: VirtualParameterCompletionStrategy::StreamIndividualThenCombine,
        }]
    }

    fn render(
        &self,
        _tool_name: &str,
        param_name: &str,
        param_value: &str,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        // Render individual parameters during streaming (same as current DiffParameterRenderer)
        match param_name {
            "old_text" => render_edit_text_block(param_value, true, theme),
            "new_text" => render_edit_text_block(param_value, false, theme),
            _ => div().child("Unknown parameter").into_any(),
        }
    }

    fn render_virtual_parameter(
        &self,
        tool_name: &str,
        virtual_param_name: &str,
        source_params: &HashMap<String, String>,
        theme: &gpui_component::theme::Theme,
    ) -> Option<gpui::AnyElement> {
        if virtual_param_name == "diff" && tool_name == "edit" {
            if let (Some(old_text), Some(new_text)) =
                (source_params.get("old_text"), source_params.get("new_text"))
            {
                // Create a synthetic diff section and render it
                let section = DiffSection {
                    search_content: old_text.clone(),
                    replace_content: new_text.clone(),
                    in_search: false,
                    in_replace: false,
                };

                Some(render_enhanced_diff_section(section, theme))
            } else {
                None
            }
        } else {
            None
        }
    }

    fn is_full_width(&self, _tool_name: &str, param_name: &str) -> bool {
        matches!(param_name, "old_text" | "new_text" | "diff")
    }
}

/// Render a text block for the edit tool (old_text or new_text)
fn render_edit_text_block(
    text: &str,
    is_deletion: bool,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    // Choose colors based on whether this is old_text (deletion) or new_text (addition)
    let (border_color, text_color) = if is_deletion {
        // Red for deletions (old_text)
        if theme.is_dark() {
            (rgb(0xCC5555), rgb(0xFFBBBB))
        } else {
            (rgb(0xCC3333), rgb(0xAA0000))
        }
    } else {
        // Green for additions (new_text)
        if theme.is_dark() {
            (rgb(0x55CC55), rgb(0xBBFFBB))
        } else {
            (rgb(0x33AA33), rgb(0x007700))
        }
    };

    div()
        .rounded_md()
        .bg(if theme.is_dark() {
            rgba(0x0A0A0AFF) // Dunklerer Hintergrund im Dark Mode
        } else {
            rgba(0xEAEAEAFF) // Hellerer Hintergrund im Light Mode
        })
        .p_2()
        .border_l_2()
        .border_color(border_color)
        .text_color(text_color)
        .text_size(px(15.))
        .font_weight(FontWeight(500.0))
        .child(text.to_string())
        .into_any()
}

/// A section of a diff with search and replace content (copied from diff_renderer.rs)
#[derive(Clone)]
struct DiffSection {
    search_content: String,
    replace_content: String,
    in_search: bool,
    in_replace: bool,
}

/// Render a diff section with enhanced visualization (adapted from diff_renderer.rs)
fn render_enhanced_diff_section(
    section: DiffSection,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    if section.in_search || section.in_replace {
        // For streaming blocks we use the simple rendering
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

/// Create a list of diff lines with change information
fn create_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    use similar::{ChangeTag, TextDiff};

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

/// Helper function to group consecutive lines of the same type
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

/// Helper function specifically for rendering streaming diff blocks
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_diff_renderer_supported_parameters() {
        let renderer = EditDiffRenderer;
        let supported = renderer.supported_parameters();

        assert_eq!(supported.len(), 2);
        assert!(supported.contains(&("edit".to_string(), "old_text".to_string())));
        assert!(supported.contains(&("edit".to_string(), "new_text".to_string())));
    }

    #[test]
    fn test_edit_diff_renderer_virtual_parameters() {
        let renderer = EditDiffRenderer;
        let virtual_params = renderer.virtual_parameters();

        assert_eq!(virtual_params.len(), 1);
        let spec = &virtual_params[0];
        assert_eq!(spec.virtual_name, "diff");
        assert_eq!(spec.source_params, vec!["old_text", "new_text"]);
        assert!(matches!(
            spec.completion_strategy,
            VirtualParameterCompletionStrategy::StreamIndividualThenCombine
        ));
    }

    // Note: Testing render methods requires a full GPUI context,
    // so we only test the logic parts here

    #[test]
    fn test_edit_diff_renderer_is_full_width() {
        let renderer = EditDiffRenderer;

        assert!(renderer.is_full_width("edit", "old_text"));
        assert!(renderer.is_full_width("edit", "new_text"));
        assert!(renderer.is_full_width("edit", "diff"));
        assert!(!renderer.is_full_width("edit", "other_param"));
    }

    #[test]
    fn test_virtual_parameter_deduplication() {
        use crate::ui::gpui::parameter_renderers::{ParameterRendererRegistry, DefaultParameterRenderer};

        // Create a registry and register the EditDiffRenderer
        let mut registry = ParameterRendererRegistry::new(Box::new(DefaultParameterRenderer));
        registry.register_renderer(Box::new(EditDiffRenderer));

        // Get virtual parameters for edit tool
        let virtual_params = registry.get_virtual_parameters_for_tool("edit");

        // Should only get one virtual parameter spec, not two (one for each source param)
        assert_eq!(virtual_params.len(), 1);
        assert_eq!(virtual_params[0].virtual_name, "diff");
        assert_eq!(virtual_params[0].source_params, vec!["old_text", "new_text"]);
    }
}
