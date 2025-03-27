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
            FileUpdaterError::SearchBlockNotFound(index, search_text) => {
                writeln!(f, "Could not find SEARCH block with index {} in the file contents", index)?;
                // Add helpful message to debug the issue
                if !search_text.is_empty() {
                    // Show the first 100 chars of search text to help diagnose the issue
                    let display_text = if search_text.len() > 100 {
                        format!("{}...", &search_text[..100])
                    } else {
                        search_text.clone()
                    };
                    writeln!(f, "Search text: ```{}```", display_text)?;
                }
                Ok(())
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

// Helper function to visualize character differences for debugging
fn show_text_diff(expected: &str, actual: &str, max_len: usize) -> String {
    let mut result = String::new();
    let expected_chars: Vec<char> = expected.chars().take(max_len).collect();
    let actual_chars: Vec<char> = actual.chars().take(max_len).collect();
    
    result.push_str("Expected: '");
    for c in &expected_chars {
        if *c == '\n' {
            result.push_str("\\n");
        } else if *c == '\r' {
            result.push_str("\\r");
        } else if *c == '\t' {
            result.push_str("\\t");
        } else if c.is_whitespace() {
            result.push_str("␣"); // Use visible space character
        } else {
            result.push(*c);
        }
    }
    result.push_str("'\nActual:   '");
    
    for c in &actual_chars {
        if *c == '\n' {
            result.push_str("\\n");
        } else if *c == '\r' {
            result.push_str("\\r");
        } else if *c == '\t' {
            result.push_str("\\t");
        } else if c.is_whitespace() {
            result.push_str("␣"); // Use visible space character
        } else {
            result.push(*c);
        }
    }
    result.push('\'');
    
    // Highlight differences with a marker line
    result.push_str("\nDiff:     '");
    let max_idx = std::cmp::min(expected_chars.len(), actual_chars.len());
    for i in 0..max_idx {
        if expected_chars[i] != actual_chars[i] {
            result.push('^');
        } else {
            result.push(' ');
        }
    }
    result.push('\'');
    
    result
}

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
                // Check for CRLF vs LF line ending issues
                let alt_search = replacement.search.replace("\n", "\r\n");
                let alt_matches: Vec<_> = result.match_indices(&alt_search).collect();
                
                if !alt_matches.is_empty() {
                    // Found matches with different line endings - use them instead
                    let (pos, _) = alt_matches[0];
                    let alt_replace = replacement.replace.replace("\n", "\r\n");
                    result.replace_range(pos..pos + alt_search.len(), &alt_replace);
                    continue;
                }
                
                // Still no match - try to provide helpful error info
                let mut error_message = format!("Could not find exact match for search block {}", index);
                
                // Get the first 100 chars of content to help diagnose
                // Commented out to avoid unused variable warning, but kept for future enhancements
                /*
                let content_sample = if result.len() > 200 {
                    format!("{}...", &result[..200])
                } else {
                    result.clone()
                };
                */
                
                // Try to find a similar section to help diagnose the issue
                let search_sample = if replacement.search.len() > 10 {
                    &replacement.search[..10]
                } else {
                    &replacement.search
                };
                
                if let Some(pos) = result.find(search_sample) {
                    let start = pos.saturating_sub(10);
                    let end = std::cmp::min(pos + search_sample.len() + 50, result.len());
                    error_message.push_str(&format!("\n\nFound similar text at position {}. Context:\n```{}```", 
                        pos, &result[start..end]));
                    
                    // Show detailed character comparison
                    let expected_len = std::cmp::min(replacement.search.len(), 100);
                    let actual_len = std::cmp::min(result.len() - pos, 100);
                    let actual_text = &result[pos..pos+actual_len];
                    
                    error_message.push_str("\n\nDetailed comparison:\n");
                    error_message.push_str(&show_text_diff(&replacement.search[..expected_len], actual_text, 100));
                }
                
                return Err(FileUpdaterError::Other(error_message).into());
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
            Err("Could not find exact match"), // Partial string match is fine for the test
        ),
        // Test handling different line endings
        (
            "line 1\r\nline 2\r\nline 3",
            vec![FileReplacement {
                search: "line 1\nline 2".to_string(),
                replace: "replaced".to_string(),
            }],
            Ok("replaced\r\nline 3"),
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
