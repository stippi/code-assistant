use crate::types::FileUpdate;

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
    let lines: Vec<&str> = content.lines().collect();

    // Validate the updates
    for update in updates {
        if update.start_line == 0 || update.end_line == 0 {
            anyhow::bail!("Line numbers must start at 1");
        }
        if update.start_line > update.end_line {
            anyhow::bail!("Start line must not be greater than end line");
        }
        if update.end_line > lines.len() {
            anyhow::bail!(
                "End line {} exceeds file length {}",
                update.end_line,
                lines.len()
            );
        }
    }

    // Sort the updates by start_line in reverse order
    let mut sorted_updates = updates.to_vec();
    sorted_updates.sort_by(|a, b| b.start_line.cmp(&a.start_line));

    // Check if there are any overlapping updates
    for updates in sorted_updates.windows(2) {
        if updates[1].end_line >= updates[0].start_line {
            anyhow::bail!(
                "Overlapping updates: lines {}-{} and {}-{}",
                updates[1].start_line,
                updates[1].end_line,
                updates[0].start_line,
                updates[0].end_line
            );
        }
    }

    // Apply the updates from bottom to top
    let mut result = content.to_string(); // Keep the original line breaks
    for update in sorted_updates {
        let start_index = if update.start_line > 1 {
            // Find the position after the previous line's newline
            result
                .split('\n')
                .take(update.start_line - 1)
                .map(|line| line.len() + 1) // +1 for the newline
                .sum()
        } else {
            0
        };

        let end_index = result
            .split('\n')
            .take(update.end_line)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            - if update.end_line == lines.len() { 1 } else { 0 };

        // Make sure the new content ends in a line break unless it is at the end of the file
        let mut new_content = update.new_content.clone();
        if update.end_line < lines.len() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }

        result.replace_range(start_index..end_index, &new_content);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FileUpdate;

    #[test]
    fn test_single_update() {
        let content = "Line 1\nLine 2\nLine 3\nLine 4\n";
        let updates = vec![FileUpdate {
            start_line: 2,
            end_line: 3,
            new_content: "Updated Line 2\nUpdated Line 3".to_string(),
        }];

        let result = apply_content_updates(content, &updates).unwrap();
        assert_eq!(result, "Line 1\nUpdated Line 2\nUpdated Line 3\nLine 4\n");
    }

    #[test]
    fn test_multiple_updates() {
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n";
        let updates = vec![
            FileUpdate {
                start_line: 1,
                end_line: 2,
                new_content: "Updated Line 1\nUpdated Line 2".to_string(),
            },
            FileUpdate {
                start_line: 4,
                end_line: 5,
                new_content: "Updated Line 4\nUpdated Line 5".to_string(),
            },
        ];

        let result = apply_content_updates(content, &updates).unwrap();
        assert_eq!(
            result,
            "Updated Line 1\nUpdated Line 2\nLine 3\nUpdated Line 4\nUpdated Line 5\n"
        );
    }

    #[test]
    fn test_invalid_line_number() {
        let content = "Line 1\n";
        let updates = vec![FileUpdate {
            start_line: 0,
            end_line: 1,
            new_content: "Updated Line".to_string(),
        }];

        let result = apply_content_updates(content, &updates);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Line numbers must start at 1"
        );
    }

    #[test]
    fn test_out_of_bounds() {
        let content = "Line 1\n";
        let updates = vec![FileUpdate {
            start_line: 1,
            end_line: 2,
            new_content: "Updated Line".to_string(),
        }];

        let result = apply_content_updates(content, &updates);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "End line 2 exceeds file length 1"
        );
    }

    #[test]
    fn test_overlapping_updates() {
        let content = "Line 1\nLine 2\nLine 3\nLine 4\n";
        let updates = vec![
            FileUpdate {
                start_line: 1,
                end_line: 2,
                new_content: "Updated Lines 1-2".to_string(),
            },
            FileUpdate {
                start_line: 2,
                end_line: 3,
                new_content: "Updated Lines 2-3".to_string(),
            },
        ];

        let result = apply_content_updates(content, &updates);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Overlapping updates"));
    }
}
