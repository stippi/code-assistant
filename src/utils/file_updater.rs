use crate::types::FileReplacement;
use crate::utils::encoding;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FileUpdaterError {
    SearchBlockNotFound(usize, String),
    MultipleMatches(usize, usize, String),
    Other(String),
}

impl std::fmt::Display for FileUpdaterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileUpdaterError::SearchBlockNotFound(index, ..) => {
                write!(
                    f,
                    "Could not find SEARCH block with index {} in the file contents",
                    index
                )
            }
            FileUpdaterError::MultipleMatches(count, index, _) => {
                write!(f, "Found {} occurrences of SEARCH block with index {}\nA SEARCH block must match exactly one location. Try enlarging the section to replace.", count, index)
            }
            FileUpdaterError::Other(msg) => {
                write!(f, "{}", msg)
            }
        }
    }
}

impl std::error::Error for FileUpdaterError {}

/// Apply replacements with content normalization to make SEARCH blocks more robust
/// against whitespace and line ending differences
pub fn apply_replacements_normalized(
    content: &str,
    replacements: &[FileReplacement],
) -> Result<String, anyhow::Error> {
    // Normalize the input content first
    let normalized_content = encoding::normalize_content(content);
    let mut result = normalized_content.clone();

    for (index, replacement) in replacements.iter().enumerate() {
        // Normalize the search string as well
        let normalized_search = encoding::normalize_content(&replacement.search);

        // Count occurrences to ensure uniqueness
        let matches: Vec<_> = result.match_indices(&normalized_search).collect();

        match matches.len() {
            0 => {
                return Err(FileUpdaterError::SearchBlockNotFound(
                    index,
                    replacement.search.clone(),
                )
                .into())
            }
            1 => {
                let (pos, _) = matches[0];
                result.replace_range(pos..pos + normalized_search.len(), &replacement.replace);
            }
            _ => {
                return Err(FileUpdaterError::MultipleMatches(
                    matches.len(),
                    index,
                    replacement.search.clone(),
                )
                .into())
            }
        }
    }

    Ok(result)
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
            }],
            Ok("Hi there\nNew is a test\nGoodbye"),
        ),
        // Test with different line endings
        (
            "function test() {\r\n  console.log('test');\r\n}", // CRLF endings
            vec![FileReplacement {
                search: "function test() {\n  console.log('test');\n}".to_string(), // LF endings
                replace: "function answer() {\n  return 42;\n}".to_string(),
            }],
            Ok("function answer() {\n  return 42;\n}"),
        ),
        // Test with both line ending and whitespace differences
        (
            "test line  \r\nwith trailing space \r\nand CRLF endings",
            vec![FileReplacement {
                search: "test line\nwith trailing space\nand CRLF endings".to_string(),
                replace: "replaced content".to_string(),
            }],
            Ok("replaced content"),
        ),
    ];

    for (input, replacements, expected) in test_cases {
        let result = apply_replacements_normalized(input, &replacements);
        match (result, expected) {
            (Ok(result), Ok(expected)) => assert_eq!(result, expected),
            (Err(e), Err(expected)) => assert!(e.to_string().contains(expected)),
            _ => panic!("Test case result did not match expected outcome"),
        }
    }

    Ok(())
}
