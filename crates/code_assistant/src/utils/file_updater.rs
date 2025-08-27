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
                write!(f, "SEARCH blocks with indices {index1} and {index2} have overlapping matches")
            }
            FileUpdaterError::AdjacentMatches(index1, index2) => {
                write!(f, "SEARCH blocks with indices {index1} and {index2} have adjacent matches")
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
        let matches: Vec<_> = normalized_content.match_indices(&normalized_search).collect();

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
