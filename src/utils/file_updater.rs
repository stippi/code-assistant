use crate::types::FileReplacement;

pub fn apply_replacements(
    content: &str,
    replacements: &[FileReplacement],
) -> Result<String, anyhow::Error> {
    let mut result = content.to_string();

    for replacement in replacements {
        // Count occurrences to ensure uniqueness
        let matches: Vec<_> = result.match_indices(&replacement.search).collect();

        match matches.len() {
            0 => anyhow::bail!(
                "Could not find search content:\n{}\nin file content",
                replacement.search
            ),
            1 => {
                let (pos, _) = matches[0];
                result.replace_range(
                    pos..pos + replacement.search.len(),
                    &replacement.replace
                );
            },
            _ => anyhow::bail!(
                "Found {} occurrences of search content:\n```\n{}\n```\nSearch text must match exactly one location. Try enlarging the section to replace.",
                matches.len(),
                replacement.search
            ),
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
            Err("Found 3 occurrences of search content"), // Partial string match is fine for the test
        ),
        // Test error with not found content
        (
            "test content",
            vec![FileReplacement {
                search: "not found".to_string(),
                replace: "anything".to_string(),
            }],
            Err("Could not find search content"), // Partial string match is fine for the test
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
