use crate::types::FileUpdate;
use std::ops::Range;

/// Represents a line in the content with its range in bytes
#[derive(Debug)]
struct LineInfo {
    /// Byte range in the content, excluding line ending
    range: Range<usize>,
    /// Whether this line ends with \r\n
    is_crlf: bool,
}

/// Applies a series of updates to a string content and returns the modified content.
/// The function preserves line endings of the original content.
///
/// # Arguments
/// * `content` - The original content to update
/// * `updates` - A slice of FileUpdate structs describing the changes
///
/// # Returns
/// * `Result<String>` - The modified content if successful
///
/// # Errors
/// * If line numbers are invalid (0 or out of bounds)
/// * If start_line > end_line
/// * If updates overlap
pub fn apply_content_updates(
    content: &str,
    updates: &[FileUpdate],
) -> Result<String, anyhow::Error> {
    // Build line index by scanning the content once
    let line_infos = index_lines(content);

    // Validate updates
    validate_updates(updates, line_infos.len())?;

    // Sort updates in reverse order to apply from bottom to top
    let mut sorted_updates = updates.to_vec();
    sorted_updates.sort_by(|a, b| b.start_line.cmp(&a.start_line));

    // Apply updates
    let mut result = content.to_string();
    for update in sorted_updates {
        apply_single_update(&mut result, &update, &line_infos)?;
    }

    Ok(result)
}

/// Creates an index of all lines in the content by scanning once through the string
fn index_lines(content: &str) -> Vec<LineInfo> {
    let mut line_infos = Vec::new();
    let mut line_start = 0;
    let mut chars = content.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == '\n' {
            // Check if this is part of CRLF
            let is_crlf = line_start < i && content[line_start..i].ends_with('\r');
            let line_end = if is_crlf { i - 1 } else { i };

            line_infos.push(LineInfo {
                range: line_start..line_end,
                is_crlf,
            });

            line_start = i + 1;
        }
    }

    // Handle last line if it doesn't end with a newline
    if line_start < content.len() {
        let is_crlf = content[line_start..].ends_with("\r\n");
        let line_end = if is_crlf {
            content.len() - 2
        } else {
            content.len()
        };

        line_infos.push(LineInfo {
            range: line_start..line_end,
            is_crlf,
        });
    } else if line_start == content.len() {
        // Handle empty last line
        line_infos.push(LineInfo {
            range: line_start..line_start,
            is_crlf: false,
        });
    }

    line_infos
}

/// Validates all updates before applying any changes
fn validate_updates(updates: &[FileUpdate], line_count: usize) -> Result<(), anyhow::Error> {
    for update in updates {
        if update.start_line == 0 || update.end_line == 0 {
            anyhow::bail!("Line numbers must start at 1");
        }
        if update.start_line > update.end_line {
            anyhow::bail!("Start line must not be greater than end line");
        }
        if update.end_line > line_count {
            anyhow::bail!(
                "End line {} exceeds file length {}",
                update.end_line,
                line_count
            );
        }
    }

    // Check for overlapping updates
    let mut sorted_updates = updates.to_vec();
    sorted_updates.sort_by(|a, b| a.start_line.cmp(&b.start_line));

    for updates in sorted_updates.windows(2) {
        if updates[0].end_line >= updates[1].start_line {
            anyhow::bail!(
                "Overlapping updates: lines {}-{} and {}-{}",
                updates[0].start_line,
                updates[0].end_line,
                updates[1].start_line,
                updates[1].end_line
            );
        }
    }

    Ok(())
}

/// Normalizes line endings in the update content to match the target line's format.
/// Also handles empty lines at the beginning and end of the update intelligently.
fn normalize_update_content(
    update: &FileUpdate,
    line_infos: &[LineInfo],
    content: &str,
) -> Result<String, anyhow::Error> {
    let original_uses_crlf = if update.start_line > 0 && update.start_line <= line_infos.len() {
        line_infos[update.start_line - 1].is_crlf
    } else {
        false
    };

    let line_ending = if original_uses_crlf { "\r\n" } else { "\n" };

    // Split content into lines, preserving empty lines but removing line endings
    let update_lines: Vec<&str> = update
        .new_content
        .split_inclusive('\n')
        .map(|l| l.trim_end_matches(&['\r', '\n']))
        .collect();

    if update_lines.is_empty() {
        return Ok(String::new());
    }

    let mut result = String::with_capacity(update.new_content.len());

    // Handle empty lines at the start
    let mut leading_empty = 0;
    for line in &update_lines {
        if line.is_empty() {
            leading_empty += 1;
        } else {
            break;
        }
    }

    // Add at most one empty line at the start if needed
    if leading_empty > 0 && update.start_line > 1 {
        let prev_line_idx = update.start_line - 2;
        let prev_line = &content[line_infos[prev_line_idx].range.clone()];
        if !prev_line.trim().is_empty() {
            result.push_str(line_ending);
        }
    }

    // Process the main content
    let mut last_non_empty = update_lines.len();
    for (i, line) in update_lines.iter().enumerate().skip(leading_empty) {
        if !line.is_empty() {
            last_non_empty = i;
        }
        if i > leading_empty && !result.ends_with(line_ending) {
            result.push_str(line_ending);
        }
        result.push_str(line);
    }

    // Handle empty lines at the end
    if last_non_empty < update_lines.len() - 1 && update.end_line < line_infos.len() {
        let next_line = &content[line_infos[update.end_line].range.clone()];
        if !next_line.trim().is_empty() {
            result.push_str(line_ending);
        }
    }

    // Ensure content ends with line ending if not the last line
    if update.end_line < line_infos.len() && !result.ends_with(line_ending) {
        result.push_str(line_ending);
    }

    Ok(result)
}

/// Applies a single update to the content
fn apply_single_update(
    content: &mut String,
    update: &FileUpdate,
    line_infos: &[LineInfo],
) -> Result<(), anyhow::Error> {
    let start_idx = if update.start_line > 1 {
        // Get the end of the previous line including its line ending
        let prev_line = &line_infos[update.start_line - 2];
        let mut end_idx = prev_line.range.end;
        if prev_line.is_crlf {
            end_idx += 2; // \r\n
        } else {
            end_idx += 1; // \n
        }
        end_idx
    } else {
        0
    };

    let end_line = &line_infos[update.end_line - 1];
    let mut end_idx = end_line.range.end;
    if end_line.is_crlf {
        end_idx += 2;
    } else if update.end_line < line_infos.len() {
        end_idx += 1;
    }

    // Normalize the update content
    let new_content = normalize_update_content(update, line_infos, content)?;

    content.replace_range(start_idx..end_idx, &new_content);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FileUpdate;

    #[test]
    fn test_single_line_updates() {
        let test_cases = vec![
            (
                "Hello\nWorld\n",
                vec![FileUpdate {
                    start_line: 1,
                    end_line: 1,
                    new_content: "Modified".to_string(),
                }],
                "Modified\nWorld\n",
            ),
            (
                "First\nSecond\nThird\n",
                vec![FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "New Second".to_string(),
                }],
                "First\nNew Second\nThird\n",
            ),
        ];

        for (input, updates, expected) in test_cases {
            let result = apply_content_updates(input, &updates).unwrap();
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_multiple_line_updates() {
        let test_cases = vec![
            (
                "One\nTwo\nThree\nFour\n",
                vec![FileUpdate {
                    start_line: 2,
                    end_line: 3,
                    new_content: "Updated\nLines".to_string(),
                }],
                "One\nUpdated\nLines\nFour\n",
            ),
            (
                "A\nB\nC\nD\nE\n",
                vec![
                    FileUpdate {
                        start_line: 1,
                        end_line: 2,
                        new_content: "First\nUpdate".to_string(),
                    },
                    FileUpdate {
                        start_line: 4,
                        end_line: 5,
                        new_content: "Second\nUpdate".to_string(),
                    },
                ],
                "First\nUpdate\nC\nSecond\nUpdate\n",
            ),
        ];

        for (input, updates, expected) in test_cases {
            let result = apply_content_updates(input, &updates).unwrap();
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_crlf_line_endings() {
        let input = "Line 1\r\nLine 2\r\nLine 3\r\n";
        let updates = vec![FileUpdate {
            start_line: 2,
            end_line: 2,
            new_content: "Modified Line".to_string(),
        }];

        let result = apply_content_updates(input, &updates).unwrap();
        assert_eq!(result, "Line 1\r\nModified Line\r\nLine 3\r\n");
    }

    #[test]
    fn test_mixed_line_endings() {
        let input = "Line 1\nLine 2\r\nLine 3\n";
        let updates = vec![
            FileUpdate {
                start_line: 1,
                end_line: 1,
                new_content: "Modified 1".to_string(),
            },
            FileUpdate {
                start_line: 2,
                end_line: 2,
                new_content: "Modified 2".to_string(),
            },
        ];

        let result = apply_content_updates(input, &updates).unwrap();
        assert_eq!(result, "Modified 1\nModified 2\r\nLine 3\n");
    }

    #[test]
    fn test_last_line_without_newline() {
        let input = "Line 1\nLine 2\nLine 3";
        let updates = vec![FileUpdate {
            start_line: 3,
            end_line: 3,
            new_content: "Modified Last".to_string(),
        }];

        let result = apply_content_updates(input, &updates).unwrap();
        assert_eq!(result, "Line 1\nLine 2\nModified Last");
    }

    #[test]
    fn test_unicode_content() {
        let input = "Hello 👋\nWorld 🌎\nTest 🧪\n";
        let updates = vec![FileUpdate {
            start_line: 2,
            end_line: 2,
            new_content: "Modified 🚀".to_string(),
        }];

        let result = apply_content_updates(input, &updates).unwrap();
        assert_eq!(result, "Hello 👋\nModified 🚀\nTest 🧪\n");
    }

    #[test]
    fn test_empty_lines() {
        let input = "First\n\nThird\n";
        let updates = vec![FileUpdate {
            start_line: 2,
            end_line: 2,
            new_content: "Second".to_string(),
        }];

        let result = apply_content_updates(input, &updates).unwrap();
        assert_eq!(result, "First\nSecond\nThird\n");
    }

    #[test]
    fn test_large_file_simulation() {
        // Create a large file content (100 lines)
        let content: String = (1..=100).map(|i| format!("Line {}\n", i)).collect();

        // Create 10 random updates
        let updates: Vec<FileUpdate> = vec![(5, 7), (20, 22), (40, 41), (60, 63), (80, 82)]
            .into_iter()
            .map(|(start, end)| FileUpdate {
                start_line: start,
                end_line: end,
                new_content: format!("Modified lines {}-{}\n", start, end),
            })
            .collect();

        // Apply updates
        let result = apply_content_updates(&content, &updates).unwrap();

        // Verify some basic properties
        assert!(result.lines().count() >= 90); // At least 90 lines (some updates might combine lines)
        assert!(updates.iter().all(|update| {
            result.contains(&format!(
                "Modified lines {}-{}",
                update.start_line, update.end_line
            ))
        }));
    }

    #[test]
    fn test_normalize_line_endings() {
        let test_cases = vec![
            (
                "Line 1\r\nLine 2\r\nLine 3\r\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "New\nLine\n".to_string(),
                },
                "Line 1\r\nNew\r\nLine\r\nLine 3\r\n",
            ),
            (
                "Line 1\nLine 2\nLine 3\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "New\r\nLine\r\n".to_string(),
                },
                "Line 1\nNew\nLine\nLine 3\n",
            ),
        ];

        for (input, update, expected) in test_cases {
            let result = apply_content_updates(input, &[update]).unwrap();
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_empty_line_handling() {
        let test_cases = vec![
            // Case 1: Empty line at start, no empty line before
            (
                "Text 1\nText 2\nText 3\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "\nNew Text\n".to_string(),
                },
                "Text 1\n\nNew Text\nText 3\n",
            ),
            // Case 2: Empty line at start, empty line already exists before
            (
                "Text 1\n\nText 2\nText 3\n",
                FileUpdate {
                    start_line: 3,
                    end_line: 3,
                    new_content: "\nNew Text\n".to_string(),
                },
                "Text 1\n\nNew Text\nText 3\n",
            ),
            // Case 3: Empty line at end, no empty line after
            (
                "Text 1\nText 2\nText 3\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "New Text\n\n".to_string(),
                },
                "Text 1\nNew Text\n\nText 3\n",
            ),
            // Case 4: Empty line at end, empty line already exists after
            (
                "Text 1\nText 2\n\nText 3\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "New Text\n\n".to_string(),
                },
                "Text 1\nNew Text\n\nText 3\n",
            ),
            // Case 5: Multiple empty lines
            (
                "Text 1\nText 2\nText 3\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "\n\nNew Text\n\n\n".to_string(),
                },
                "Text 1\n\nNew Text\n\nText 3\n",
            ),
            // Case 6: Mixed line endings with empty lines
            (
                "Text 1\r\nText 2\r\nText 3\r\n",
                FileUpdate {
                    start_line: 2,
                    end_line: 2,
                    new_content: "\nNew Text\n\n".to_string(),
                },
                "Text 1\r\n\r\nNew Text\r\n\r\nText 3\r\n",
            ),
        ];

        for (input, update, expected) in test_cases {
            let result = apply_content_updates(input, &[update]).unwrap();
            assert_eq!(result, expected, "Failed for input:\n{}", input);
        }
    }

    #[test]
    fn test_complex_mixed_cases() {
        let test_cases = vec![
            // Mixed line endings with empty lines at both ends
            (
                "Header\r\n\r\nContent\r\nFooter",
                vec![FileUpdate {
                    start_line: 3,
                    end_line: 3,
                    new_content: "\n\nNew Content\n\n".to_string(),
                }],
                "Header\r\n\r\nNew Content\r\n\r\nFooter",
            ),
            // Multiple updates
            (
                "Line 1\nLine 2\nLine 3\nLine 4",
                vec![
                    FileUpdate {
                        start_line: 2,
                        end_line: 2,
                        new_content: "\nNew Line 2\n".to_string(),
                    },
                    FileUpdate {
                        start_line: 4,
                        end_line: 4,
                        new_content: "New Line 4\n\n".to_string(),
                    },
                ],
                "Line 1\n\nNew Line 2\nLine 3\nNew Line 4\n",
            ),
        ];

        for (input, updates, expected) in test_cases {
            let result = apply_content_updates(input, &updates).unwrap();
            assert_eq!(result, expected, "Failed for input:\n{}", input);
        }
    }
}
