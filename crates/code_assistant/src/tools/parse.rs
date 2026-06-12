//! Parsing helpers for tool inputs: paths with line ranges and
//! search/replace diff blocks. The dialect parsers (XML/Caret) live in
//! `crate::tool_dialects`.

use crate::types::ToolError;
use anyhow::Result;
use fs_explorer::FileReplacement;
use std::path::PathBuf;

/// Represents a parsed path with optional line ranges
#[derive(Debug, Clone)]
pub struct PathWithLineRange {
    pub path: PathBuf,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

impl PathWithLineRange {
    /// Parse a path string that may contain line ranges like "file.txt:10-20"
    pub fn parse(path_str: &str) -> Result<Self, ToolError> {
        // Check if the path contains a colon (not part of Windows drive letter)
        if let Some(colon_pos) = path_str.rfind(':') {
            // Skip Windows drive letter (e.g., C:)
            if colon_pos > 1
                || (colon_pos == 1 && !path_str.chars().next().unwrap().is_alphabetic())
            {
                let (file_path, line_range) = path_str.split_at(colon_pos);
                let line_range = &line_range[1..]; // Skip the colon

                // Parse the line range
                if line_range.is_empty() {
                    // Just a colon with nothing after it, treat as normal path
                    return Ok(Self {
                        path: PathBuf::from(path_str),
                        start_line: None,
                        end_line: None,
                    });
                }

                if let Some(dash_pos) = line_range.find('-') {
                    // Range with dash: file.txt:10-20
                    let (start, end) = line_range.split_at(dash_pos);
                    let end = &end[1..]; // Skip the dash

                    let start_line = if start.is_empty() {
                        None // file.txt:-20
                    } else {
                        Some(start.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid start line number: {start}"))
                        })?)
                    };

                    let end_line = if end.is_empty() {
                        None // file.txt:10-
                    } else {
                        Some(end.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid end line number: {end}"))
                        })?)
                    };

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line,
                        end_line,
                    });
                } else {
                    // Single line: file.txt:15
                    let line_num = line_range.parse::<usize>().map_err(|_| {
                        ToolError::ParseError(format!("Invalid line number: {line_range}"))
                    })?;

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line: Some(line_num),
                        end_line: Some(line_num),
                    });
                }
            }
        }

        // No line range specified
        Ok(Self {
            path: PathBuf::from(path_str),
            start_line: None,
            end_line: None,
        })
    }
}

pub(crate) fn parse_search_replace_blocks(
    content: &str,
) -> Result<Vec<FileReplacement>, ToolError> {
    let mut replacements = Vec::new();
    let mut lines = content.lines().peekable();
    let mut had_valid_block = false;

    // Skip leading empty lines
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    // Check first non-empty line is a start marker
    if let Some(line) = lines.peek() {
        let trimmed = line.trim_end();
        if !trimmed.starts_with("<<<<<<< SEARCH") {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content before diff markers".to_string(),
            ));
        }
    } else {
        // Empty content
        return Err(ToolError::ParseError(
            "Malformed diff: No search/replace blocks found. Expecting content to start with <<<<<<< SEARCH".to_string(),
        ));
    }

    while let Some(line) = lines.next() {
        // Skip empty lines between blocks
        if line.trim().is_empty() {
            continue;
        }

        // Match the exact marker without trimming leading whitespace
        let is_search_all = line.trim_end() == "<<<<<<< SEARCH_ALL";
        let is_search = line.trim_end() == "<<<<<<< SEARCH";

        if is_search || is_search_all {
            had_valid_block = true;
            let mut search = String::new();
            let mut replace = String::new();
            let mut found_separator = false;

            // Collect search content until we find the separator
            for line in lines.by_ref() {
                if line.trim_end() == "=======" {
                    found_separator = true;
                    break;
                }
                if !search.is_empty() {
                    search.push('\n');
                }
                search.push_str(line);
            }

            if !found_separator {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing separator marker (=======)".to_string(),
                ));
            }

            // Collect replace content
            let end_marker = if is_search_all {
                ">>>>>>> REPLACE_ALL"
            } else {
                ">>>>>>> REPLACE"
            };
            let mut found_end_marker = false;

            // Before collecting the replace content, we'll check if there are
            // additional separator markers in the remaining content
            {
                // Clone the iterator to peek ahead without consuming it
                let preview_iter = lines.clone();
                let mut lines_to_end_marker = Vec::new();
                let mut reached_end_marker = false;

                // Collect all lines until end marker
                for line in preview_iter {
                    if line.trim_end() == end_marker {
                        reached_end_marker = true;
                        break;
                    }
                    lines_to_end_marker.push(line);
                }

                if !reached_end_marker {
                    return Err(ToolError::ParseError(
                        "Malformed diff: Missing closing marker".to_string(),
                    ));
                }

                // Check for invalid separators
                let separator_count = lines_to_end_marker
                    .iter()
                    .filter(|line| line.trim_end() == "=======")
                    .count();

                // Special case: allow one separator if it's the last line before end marker
                if separator_count > 0 {
                    let last_line = lines_to_end_marker.last();

                    if separator_count > 1
                        || (last_line.is_some_and(|line| line.trim_end() != "======="))
                    {
                        return Err(ToolError::ParseError(
                            "Malformed diff: Multiple separator markers (=======) found in the content. This is not allowed as it would make it impossible to edit files containing separators.".to_string(),
                        ));
                    }
                }
            }

            // Now actually process the replace content
            while let Some(current_line) = lines.next() {
                // Check for end marker
                if current_line.trim_end() == end_marker {
                    found_end_marker = true;
                    break;
                }

                // Check if this is a separator right before end marker
                if current_line.trim_end() == "=======" {
                    if let Some(next_line) = lines.peek() {
                        if next_line.trim_end() == end_marker {
                            // Skip this separator if it's right before the end marker
                            continue;
                        }
                    }

                    // This should never happen due to our check above, but just in case
                    return Err(ToolError::ParseError(
                        "Malformed diff: Found separator marker (=======) in replace content. This is not allowed as it would make subsequent edits impossible.".to_string(),
                    ));
                }

                // Regular content line - add to replace content
                if !replace.is_empty() {
                    replace.push('\n');
                }
                replace.push_str(current_line);
            }

            if !found_end_marker {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing closing marker (>>>>>>> REPLACE)".to_string(),
                ));
            }

            replacements.push(FileReplacement {
                search,
                replace,
                replace_all: is_search_all,
            });
        } else {
            // Found a non-empty line that isn't a start marker
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content between diff blocks".to_string(),
            ));
        }
    }

    // Check for non-whitespace content after all blocks are processed
    for line in lines {
        if !line.trim().is_empty() {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content after diff blocks".to_string(),
            ));
        }
    }

    if !had_valid_block {
        return Err(ToolError::ParseError(
            "Malformed diff: No valid search/replace blocks found".to_string(),
        ));
    }

    Ok(replacements)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_replace_blocks_normal() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    return a;\n",
            "}\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    return a;\n}");
        assert!(!result[0].replace_all);
    }

    #[test]
    fn test_parse_search_replace_blocks_multiple() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "=======\n",
            "if a >= b {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH\n",
            "return a;\n",
            "=======\n",
            "return a + 1;\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "if a > b {");
        assert_eq!(result[0].replace, "if a >= b {");
        assert_eq!(result[1].search, "return a;");
        assert_eq!(result[1].replace, "return a + 1;");
    }

    #[test]
    fn test_parse_search_replace_blocks_with_second_separator() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    // Add a comment\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    // Add a comment");
    }

    #[test]
    fn test_parse_search_replace_blocks_empty_sections() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "// This comment will be removed\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "// This comment will be removed");
        assert_eq!(result[0].replace, "");
    }

    #[test]
    fn test_parse_search_replace_all_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "console.log(");
        assert_eq!(result[0].replace, "logger.debug(");
        assert!(result[0].replace_all);
    }

    #[test]
    fn test_parse_mixed_search_replace_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert!(!result[0].replace_all);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert!(result[1].replace_all);
    }

    #[test]
    fn test_parse_multiple_search_replace_blocks_whitespace() {
        let content = concat!(
            "\n",
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "\n",
            "<<<<<<< SEARCH\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE",
            "\n",
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert!(!result[0].replace_all);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert!(!result[1].replace_all);
    }

    #[test]
    fn test_parse_malformed_diff_with_missing_closing_marker() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        content to replace with\n",
            "======="
        );

        // The diff is malformed (no closing >>>>>>> marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Missing closing marker"),
            "Error should mention the missing closing marker: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_multiple_separators() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        some more content to search\n",
            "=======\n",
            "        content to replace with\n",
            ">>>>>>> REPLACE\n",
        );

        // The diff is malformed (it has multiple separators), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(
            result.is_err(),
            "Expected an error for malformed diff with multiple separators"
        );
        let error_message = result.unwrap_err().to_string();

        assert!(
            error_message.contains("Multiple separator markers"),
            "Error should mention the problem with multiple separator markers: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_missing_start_marker() {
        let content = concat!(
            "Some regular content\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (no start <<<<<<< marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("content before diff markers"),
            "Error should mention unexpected content: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_between_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content between blocks\n",
            "<<<<<<< SEARCH\n",
            "second search\n",
            "=======\n",
            "second replace\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (non-whitespace content between blocks), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content between blocks: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_after_last_block() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content after the last block"
        );

        // The diff is malformed (non-whitespace content after last block), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();

        // With the current implementation, this is detected as content between blocks
        // since we don't distinguish between "after last block" and "between blocks"
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content: {error_message}"
        );
    }

}
