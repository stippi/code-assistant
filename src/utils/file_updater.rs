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

    // Ensure the new content has the correct line ending
    let mut new_content = update.new_content.clone();
    if update.end_line < line_infos.len() {
        let last_line = &line_infos[update.end_line - 1];
        if !new_content.ends_with('\n') {
            if last_line.is_crlf {
                new_content.push_str("\r\n");
            } else {
                new_content.push('\n');
            }
        }
    }

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
}
