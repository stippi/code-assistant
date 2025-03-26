use crate::ui::gpui::parameter_renderers::ParameterRenderer;
use gpui::{div, hsla, rgb, rgba, Element, FontWeight, IntoElement, ParentElement, Styled};
use similar::{ChangeTag, TextDiff};

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
    inline_changes: Option<Vec<(usize, usize, LineChangeType)>>, // (start, end, type)
}

/// Render a diff section with enhanced visualization
fn render_enhanced_diff_section(section: DiffSection) -> gpui::AnyElement {
    if section.in_search || section.in_replace {
        // Für streaming-Blöcke verwenden wir das einfache Rendering - eine neue Funktion, die direkt AnyElement zurückgibt
        return render_streaming_diff_section(section);
    }

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
        .into_any()
}

/// Create a list of diff lines with change information
fn create_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();

    // Create line-by-line diff
    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(old_text, new_text);

    // Process line changes and collect blocks of changes
    let mut deleted_block = Vec::new();
    let mut added_block = Vec::new();

    // Process all changes to identify blocks
    for change in diff.iter_all_changes() {
        let line_content = change.value().to_string();

        match change.tag() {
            ChangeTag::Equal => {
                // Process any pending blocks first
                if !deleted_block.is_empty() || !added_block.is_empty() {
                    process_diff_blocks(&mut result, &deleted_block, &added_block);
                    deleted_block.clear();
                    added_block.clear();
                }

                // Add unchanged line
                result.push(DiffLine {
                    content: line_content,
                    change_type: LineChangeType::Unchanged,
                    inline_changes: None,
                });
            }
            ChangeTag::Delete => {
                // Add to deleted block
                deleted_block.push(line_content);
            }
            ChangeTag::Insert => {
                // Add to added block
                added_block.push(line_content);
            }
        }
    }

    // Process any remaining blocks
    if !deleted_block.is_empty() || !added_block.is_empty() {
        process_diff_blocks(&mut result, &deleted_block, &added_block);
    }

    result
}

/// Process blocks of deleted and added lines to compute word-level diffs
fn process_diff_blocks(
    result: &mut Vec<DiffLine>, 
    deleted_block: &[String], 
    added_block: &[String]
) {
    // If both blocks have content, do word-level diffing between them
    if !deleted_block.is_empty() && !added_block.is_empty() {
        // Join blocks for comparison
        let deleted_text = deleted_block.join("\n");
        let added_text = added_block.join("\n");

        // Create word-level diff
        let word_diff = TextDiff::configure()
            .timeout(std::time::Duration::from_millis(100))
            .algorithm(similar::Algorithm::Myers)
            .diff_words(&deleted_text, &added_text);

        // Process deleted lines with inline highlighting
        for (i, line) in deleted_block.iter().enumerate() {
            let offset = if i > 0 {
                deleted_block[..i].join("\n").len() + 1 // +1 for newline
            } else {
                0
            };

            let inline_changes = extract_inline_changes(&word_diff, line, offset, LineChangeType::Deleted);
            
            result.push(DiffLine {
                content: line.clone(),
                change_type: LineChangeType::Deleted,
                inline_changes: if inline_changes.is_empty() { None } else { Some(inline_changes) },
            });
        }

        // Process added lines with inline highlighting
        for (i, line) in added_block.iter().enumerate() {
            let offset = if i > 0 {
                added_block[..i].join("\n").len() + 1 // +1 for newline
            } else {
                0
            };

            let inline_changes = extract_inline_changes(&word_diff, line, offset, LineChangeType::Added);
            
            result.push(DiffLine {
                content: line.clone(),
                change_type: LineChangeType::Added,
                inline_changes: if inline_changes.is_empty() { None } else { Some(inline_changes) },
            });
        }
    } else {
        // If only one type of block exists, add them without inline highlighting
        for line in deleted_block {
            result.push(DiffLine {
                content: line.clone(),
                change_type: LineChangeType::Deleted,
                inline_changes: None,
            });
        }

        for line in added_block {
            result.push(DiffLine {
                content: line.clone(),
                change_type: LineChangeType::Added,
                inline_changes: None,
            });
        }
    }
}

/// Extract inline changes from a word diff for a specific line
fn extract_inline_changes<'a>(
    word_diff: &TextDiff<'a, 'a, 'a, str>,
    line: &str,
    _line_offset: usize,
    line_type: LineChangeType,
) -> Vec<(usize, usize, LineChangeType)> {
    let mut changes = Vec::new();

    // Get the relevant tag for this line type
    let relevant_tag = match line_type {
        LineChangeType::Deleted => ChangeTag::Delete,
        LineChangeType::Added => ChangeTag::Insert,
        _ => return changes, // Should not happen
    };

    // Iterate over changes and collect those that apply to this line
    for change in word_diff.iter_all_changes() {
        if change.tag() == relevant_tag {
            // We need to find this change in our current line
            let value = change.value();
            
            // Simple string searching to find all occurrences in the line
            let mut start_idx = 0;
            while let Some(pos) = line[start_idx..].find(value) {
                let abs_pos = start_idx + pos;
                let end_pos = abs_pos + value.len();
                
                // Add this change
                changes.push((abs_pos, end_pos, line_type));
                
                // Move past this occurrence
                start_idx = abs_pos + 1;
                
                // Safety check to prevent infinite loops with empty strings
                if value.is_empty() {
                    break;
                }
            }
        }
    }

    // Sort by start position
    changes.sort_by_key(|(start, _, _)| *start);
    
    // Merge overlapping changes
    let mut i = 0;
    while i + 1 < changes.len() {
        let (start1, end1, _) = changes[i];
        let (start2, end2, _) = changes[i + 1];
        
        if start2 <= end1 {
            // Merge these changes
            changes[i] = (start1, end2.max(end1), line_type);
            changes.remove(i + 1);
        } else {
            i += 1;
        }
    }

    changes
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
        // Sicherstellen, dass die Indizes innerhalb gültiger Grenzen liegen
        if start >= content.len() || end > content.len() || start >= end {
            continue;  // Überspringe ungültige Bereiche
        }

        // Add unchanged text before this change
        if start > last_pos {
            spans.push(
                div()
                    .child(content[last_pos..start].to_string())
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
                .into_any(),
        );

        last_pos = end;
    }

    // Add any remaining text
    if last_pos < content.len() {
        spans.push(
            div()
                .child(content[last_pos..].to_string())
                .into_any(),
        );
    }

    // Return the line with all spans
    line_div.children(spans).into_any()
}

/// Helper function specifically for rendering streaming diff blocks, returns AnyElement directly
fn render_streaming_diff_section(section: DiffSection) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(vec![
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
        .into_any()
}
