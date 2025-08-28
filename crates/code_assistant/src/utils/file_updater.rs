use crate::types::FileReplacement;
use crate::utils::encoding;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FileUpdaterError {
    SearchBlockNotFound(usize, String),
    MultipleMatches(usize, usize, String),
    OverlappingMatches(usize, usize),
    AdjacentMatches(usize, usize),
    Other(String),
}

/// Represents a single match found in the content
#[derive(Debug, Clone)]
pub struct MatchRange {
    pub replacement_index: usize,
    pub match_index: usize, // For replace_all cases where one replacement has multiple matches
    pub start: usize,
    pub end: usize,
    pub matched_text: String,
}

/// Represents a range of stable (unchanged) content between matches
#[derive(Debug, Clone)]
pub struct StableRange {
    pub start: usize,
    pub end: usize,
    pub content: String,
}

impl std::fmt::Display for FileUpdaterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileUpdaterError::SearchBlockNotFound(index, ..) => {
                write!(
                    f,
                    "Could not find SEARCH block with index {index} in the file contents"
                )
            }
            FileUpdaterError::MultipleMatches(count, index, _) => {
                write!(f, "Found {count} occurrences of SEARCH block with index {index}\nA SEARCH block must match exactly one location. Try enlarging the section to replace.")
            }
            FileUpdaterError::OverlappingMatches(index1, index2) => {
                write!(
                    f,
                    "SEARCH blocks with indices {index1} and {index2} have overlapping matches"
                )
            }
            FileUpdaterError::AdjacentMatches(index1, index2) => {
                write!(
                    f,
                    "SEARCH blocks with indices {index1} and {index2} have adjacent matches"
                )
            }
            FileUpdaterError::Other(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for FileUpdaterError {}

/// Find all matches for the given replacements in the content
/// Returns a tuple of (matches, has_conflicts) where has_conflicts indicates
/// whether there are overlapping or adjacent matches that would complicate
/// format-aware parameter reconstruction
pub fn find_replacement_matches(
    content: &str,
    replacements: &[FileReplacement],
) -> Result<(Vec<MatchRange>, bool), FileUpdaterError> {
    // Normalize the input content first
    let normalized_content = encoding::normalize_content(content);
    let mut all_matches = Vec::new();

    for (replacement_index, replacement) in replacements.iter().enumerate() {
        // Normalize the search string as well
        let normalized_search = encoding::normalize_content(&replacement.search);

        // Find all occurrences
        let matches: Vec<_> = normalized_content
            .match_indices(&normalized_search)
            .collect();

        if matches.is_empty() {
            return Err(FileUpdaterError::SearchBlockNotFound(
                replacement_index,
                replacement.search.clone(),
            ));
        }

        if !replacement.replace_all && matches.len() > 1 {
            return Err(FileUpdaterError::MultipleMatches(
                matches.len(),
                replacement_index,
                replacement.search.clone(),
            ));
        }

        // Add matches to our collection
        for (match_index, (start, matched_text)) in matches.into_iter().enumerate() {
            all_matches.push(MatchRange {
                replacement_index,
                match_index,
                start,
                end: start + matched_text.len(),
                matched_text: matched_text.to_string(),
            });
        }
    }

    // Sort matches by position for conflict detection
    all_matches.sort_by_key(|m| m.start);

    // Check for overlaps and adjacencies
    let mut has_conflicts = false;
    for window in all_matches.windows(2) {
        let (curr, next) = (&window[0], &window[1]);
        if curr.end > next.start {
            return Err(FileUpdaterError::OverlappingMatches(
                curr.replacement_index,
                next.replacement_index,
            ));
        }
        if curr.end == next.start {
            has_conflicts = true;
            // Don't return error, just mark as conflicted
        }
    }

    Ok((all_matches, has_conflicts))
}

/// Apply replacements using pre-found matches
pub fn apply_matches(
    content: &str,
    matches: &[MatchRange],
    replacements: &[FileReplacement],
) -> Result<String, FileUpdaterError> {
    // Normalize the input content first
    let normalized_content = encoding::normalize_content(content);
    let mut result = normalized_content;

    // Sort matches by position in reverse order so we can apply them without
    // affecting the positions of earlier matches
    let mut sorted_matches = matches.to_vec();
    sorted_matches.sort_by_key(|m| std::cmp::Reverse(m.start));

    for match_range in sorted_matches {
        let replacement = &replacements[match_range.replacement_index];
        let normalized_replace = encoding::normalize_content(&replacement.replace);

        result.replace_range(match_range.start..match_range.end, &normalized_replace);
    }

    Ok(result)
}

/// Extract stable (unchanged) content ranges between matches
/// These ranges can be used to reconstruct formatted replacements by finding
/// the same stable content in the formatted file
pub fn extract_stable_ranges(content: &str, matches: &[MatchRange]) -> Vec<StableRange> {
    let normalized_content = encoding::normalize_content(content);
    let mut stable_ranges = Vec::new();

    // Sort matches by position to process them in order
    let mut sorted_matches = matches.to_vec();
    sorted_matches.sort_by_key(|m| m.start);

    let mut current_pos = 0;

    for match_range in &sorted_matches {
        // Add stable range before this match (if any)
        if current_pos < match_range.start {
            let stable_content = normalized_content[current_pos..match_range.start].to_string();
            // Include whitespace-only ranges as anchors to avoid shifting whitespace into replacements
            stable_ranges.push(StableRange {
                start: current_pos,
                end: match_range.start,
                content: stable_content,
            });
        }

        // Move past this match
        current_pos = match_range.end;
    }

    // Add final stable range after the last match (if any)
    if current_pos < normalized_content.len() {
        let stable_content = normalized_content[current_pos..].to_string();
        // Include whitespace-only ranges as anchors
        stable_ranges.push(StableRange {
            start: current_pos,
            end: normalized_content.len(),
            content: stable_content,
        });
    }

    stable_ranges
}

/// Reconstruct formatted replacements by finding stable ranges in formatted content
/// Returns updated replacements with formatted replace text, or None if reconstruction fails
pub fn reconstruct_formatted_replacements(
    _original_content: &str,
    formatted_content: &str,
    stable_ranges: &[StableRange],
    original_matches: &[MatchRange],
    original_replacements: &[FileReplacement],
) -> Option<Vec<FileReplacement>> {
    let normalized_formatted = encoding::normalize_content(formatted_content);

    // Try to find all stable ranges in the formatted content
    let mut stable_positions = Vec::new();
    let mut search_start = 0;

    for stable_range in stable_ranges {
        // Normalize the stable content for matching (handle potential whitespace changes)
        let normalized_stable = encoding::normalize_content(&stable_range.content);

        // Find this stable content in the formatted file
        if let Some(pos) = normalized_formatted[search_start..].find(&normalized_stable) {
            let absolute_pos = search_start + pos;
            stable_positions.push((absolute_pos, absolute_pos + normalized_stable.len()));
            search_start = absolute_pos + normalized_stable.len();
        } else {
            // Stable range not found - formatting changed supposedly stable content
            return None;
        }
    }

    // Now reconstruct the formatted replacements
    let mut updated_replacements = original_replacements.to_vec();

    // Sort matches by position for processing
    let mut sorted_matches = original_matches.to_vec();
    sorted_matches.sort_by_key(|m| m.start);

    // Group matches by replacement index
    let mut matches_by_replacement: std::collections::HashMap<usize, Vec<&MatchRange>> =
        std::collections::HashMap::new();
    for match_range in &sorted_matches {
        matches_by_replacement
            .entry(match_range.replacement_index)
            .or_default()
            .push(match_range);
    }

    // Process each replacement
    for (replacement_index, replacement_matches) in matches_by_replacement {
        if replacement_matches.len() == 1 {
            // Single match case - extract the formatted replacement text
            let match_range = replacement_matches[0];

            // Find the stable ranges that surround this match
            let before_stable = stable_ranges.iter().find(|sr| sr.end == match_range.start);
            let after_stable = stable_ranges.iter().find(|sr| sr.start == match_range.end);

            // Find corresponding positions in formatted content
            let formatted_start = if let Some(before) = before_stable {
                // Find where this stable range ends in formatted content
                if let Some(idx) = stable_ranges.iter().position(|sr| sr.start == before.start) {
                    if idx < stable_positions.len() {
                        stable_positions[idx].1
                    } else {
                        continue; // Skip this replacement
                    }
                } else {
                    0 // Start of file
                }
            } else {
                0 // Start of file
            };

            let formatted_end = if let Some(after) = after_stable {
                // Find where this stable range starts in formatted content
                if let Some(idx) = stable_ranges.iter().position(|sr| sr.start == after.start) {
                    if idx < stable_positions.len() {
                        stable_positions[idx].0
                    } else {
                        continue; // Skip this replacement
                    }
                } else {
                    normalized_formatted.len() // End of file
                }
            } else {
                normalized_formatted.len() // End of file
            };

            // Extract the formatted replacement text
            if formatted_start <= formatted_end {
                let formatted_replace =
                    normalized_formatted[formatted_start..formatted_end].to_string();
                updated_replacements[replacement_index].replace = formatted_replace;
            }
        } else {
            // Multiple matches (replace_all case) - for now, don't update these
            // This is complex because we'd need to figure out which formatted sections
            // correspond to which original matches
            continue;
        }
    }

    Some(updated_replacements)
}

/// Apply replacements with content normalization to make SEARCH blocks more robust
/// against whitespace and line ending differences
pub fn apply_replacements_normalized(
    content: &str,
    replacements: &[FileReplacement],
) -> Result<String, anyhow::Error> {
    // Use the new split functions
    let (matches, _has_conflicts) = find_replacement_matches(content, replacements)?;
    apply_matches(content, &matches, replacements).map_err(Into::into)
}

#[test]
fn test_extract_stable_ranges() -> Result<(), anyhow::Error> {
    let content = "function foo() {\n    console.log('hello');\n    return 42;\n}";
    //             01234567890123456 78901234567890123456789012 345678901234567 890
    //                                   ^21                  ^42

    let matches = vec![MatchRange {
        replacement_index: 0,
        match_index: 0,
        start: 21, // Start of "console.log('hello');" (after the 4 spaces)
        end: 42,   // End of "console.log('hello');"
        matched_text: "console.log('hello');".to_string(),
    }];

    let stable_ranges = extract_stable_ranges(content, &matches);

    // Should have stable ranges before and after the match
    assert_eq!(stable_ranges.len(), 2);

    // First stable range: "function foo() {\n    "
    assert_eq!(stable_ranges[0].start, 0);
    assert_eq!(stable_ranges[0].end, 21);
    assert_eq!(stable_ranges[0].content, "function foo() {\n    ");

    // Second stable range: "\n    return 42;\n}"
    assert_eq!(stable_ranges[1].start, 42);
    assert_eq!(stable_ranges[1].end, content.len());
    assert_eq!(stable_ranges[1].content, "\n    return 42;\n}");

    Ok(())
}

#[test]
fn test_reconstruct_formatted_replacements() -> Result<(), anyhow::Error> {
    // Use a simpler example where stable content doesn't change with formatting
    let original_content = "const x = 1;\nconst y=2;\nconst z = 3;";
    let formatted_content = "const x = 1;\nconst y = 2;\nconst z = 3;"; // Added space around =

    let matches = vec![MatchRange {
        replacement_index: 0,
        match_index: 0,
        start: 13, // Start of "const y=2;"
        end: 23,   // End of that section
        matched_text: "const y=2;".to_string(),
    }];

    let stable_ranges = extract_stable_ranges(original_content, &matches);

    let original_replacements = vec![FileReplacement {
        search: "const y=2;".to_string(),
        replace: "const y=42;".to_string(),
        replace_all: false,
    }];

    let updated_replacements = reconstruct_formatted_replacements(
        original_content,
        formatted_content,
        &stable_ranges,
        &matches,
        &original_replacements,
    );

    // Should successfully reconstruct
    if updated_replacements.is_none() {
        println!("Stable ranges: {stable_ranges:?}");
        println!("Original content: {original_content:?}");
        println!("Formatted content: {formatted_content:?}");
    }
    assert!(updated_replacements.is_some());

    Ok(())
}

#[test]
fn test_apply_replacements_normalized() -> Result<(), anyhow::Error> {
    let test_cases: Vec<(&str, Vec<FileReplacement>, Result<&str, &str>)> = vec![
        // Test with trailing whitespace
        (
            "Hello World \nThis is a test\nGoodbye",
            vec![FileReplacement {
                search: "Hello World\nThis".to_string(), // No trailing space in search
                replace: "Hi there\nNew".to_string(),
                replace_all: false,
            }],
            Ok("Hi there\nNew is a test\nGoodbye"),
        ),
        // Test with different line endings
        (
            "function test() {\r\n  console.log('test');\r\n}", // CRLF endings
            vec![FileReplacement {
                search: "function test() {\n  console.log('test');\n}".to_string(), // LF endings
                replace: "function answer() {\n  return 42;\n}".to_string(),
                replace_all: false,
            }],
            Ok("function answer() {\n  return 42;\n}"),
        ),
        // Test with both line ending and whitespace differences
        (
            "test line  \r\nwith trailing space \r\nand CRLF endings",
            vec![FileReplacement {
                search: "test line\nwith trailing space\nand CRLF endings".to_string(),
                replace: "replaced content".to_string(),
                replace_all: false,
            }],
            Ok("replaced content"),
        ),
        // Test replacing all occurrences
        (
            "log('test');\nlog('test2');\nlog('test3');",
            vec![FileReplacement {
                search: "log(".to_string(),
                replace: "console.log(".to_string(),
                replace_all: true,
            }],
            Ok("console.log('test');\nconsole.log('test2');\nconsole.log('test3');"),
        ),
        // Test error when multiple matches but replace_all is false
        (
            "log('test');\nlog('test2');\nlog('test3');",
            vec![FileReplacement {
                search: "log(".to_string(),
                replace: "console.log(".to_string(),
                replace_all: false,
            }],
            Err("Found 3 occurrences"),
        ),
    ];

    for (input, replacements, expected) in test_cases {
        let result = apply_replacements_normalized(input, &replacements);
        match (&result, &expected) {
            (Ok(res), Ok(exp)) => assert_eq!(res, exp),
            (Err(e), Err(exp)) => assert!(e.to_string().contains(exp)),
            _ => {
                panic!(
                    "Test case result did not match expected outcome:\nResult: {result:?}\nExpected: {expected:?}"
                );
            }
        }
    }

    Ok(())
}
