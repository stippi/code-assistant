use crate::types::FileReplacement;

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

pub fn apply_replacements(
    content: &str,
    replacements: &[FileReplacement],
) -> Result<String, anyhow::Error> {
    let mut result = content.to_string();

    for (index, replacement) in replacements.iter().enumerate() {
        // Count occurrences to ensure uniqueness
        let matches: Vec<_> = result.match_indices(&replacement.search).collect();

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
                result.replace_range(pos..pos + replacement.search.len(), &replacement.replace);
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
fn test_apply_replacements() -> Result<(), anyhow::Error> {
    let test_cases = vec![
        // Basic replacement
        (
            "Hello World\nThis is a test\nGoodbye",
            vec![FileReplacement {
                search: "Hello World".to_string(),
                replace: "Hi there".to_string(),
            }],
            Ok("Hi there\nThis is a test\nGoodbye"),
        ),
        // Multiple unique replacements
        (
            "function test() {\n  console.log('test');\n}",
            vec![
                FileReplacement {
                    search: "console.log('test');".to_string(),
                    replace: "return 42;".to_string(),
                },
                FileReplacement {
                    search: "function test()".to_string(),
                    replace: "function answer()".to_string(),
                },
            ],
            Ok("function answer() {\n  return 42;\n}"),
        ),
        // Test error with duplicate content
        (
            "test\ntest\ntest",
            vec![FileReplacement {
                search: "test".to_string(),
                replace: "replaced".to_string(),
            }],
            Err("Found 3 occurrences of SEARCH block"), // Partial string match is fine for the test
        ),
        // Test error with not found content
        (
            "test content",
            vec![FileReplacement {
                search: "not found".to_string(),
                replace: "anything".to_string(),
            }],
            Err("Could not find SEARCH block"), // Partial string match is fine for the test
        ),
    ];

    for (input, replacements, expected) in test_cases {
        let result = apply_replacements(input, &replacements);
        match (result, expected) {
            (Ok(result), Ok(expected)) => assert_eq!(result, expected),
            (Err(e), Err(expected)) => assert!(e.to_string().contains(expected)),
            _ => panic!("Test case result did not match expected outcome"),
        }
    }

    Ok(())
}
