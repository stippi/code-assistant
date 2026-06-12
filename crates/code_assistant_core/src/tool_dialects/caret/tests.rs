//! Caret dialect tests: invocation parsing with filters, arrays, and
//! multiline parameters.

use crate::types::ToolSyntax;
use anyhow::Result;
use llm::{ContentBlock, LLMResponse, Usage};

#[tokio::test]
async fn test_caret_parsing_with_single_tool_filter() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    let text = concat!(
        "I'll read some files first:\n\n",
        "^^^read_files\n",
        "project: test\n",
        "paths: [\n",
        "file1.txt\n",
        "file2.txt\n",
        "]\n",
        "^^^\n\n",
        "Then I'll write a new file:\n\n",
        "^^^write_file\n",
        "project: test\n",
        "path: new_file.txt\n",
        "content: test content\n",
        "^^^"
    );

    let filter = SingleToolFilter;
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should only get the first tool due to SingleToolFilter
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[0].input["project"], "test");

    // Text should be truncated after the first tool block
    assert!(truncated_text.contains("^^^read_files"));
    assert!(truncated_text.contains("^^^"));
    assert!(!truncated_text.contains("write_file"));
}

#[tokio::test]
async fn test_caret_parsing_with_smart_filter_allows_multiple_read_tools() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SmartToolFilter;

    let text = concat!(
        "Let me analyze the project:\n\n",
        "^^^read_files\n",
        "project: test\n",
        "paths: [\n",
        "package.json\n",
        "]\n",
        "^^^\n\n",
        "Now list the files:\n\n",
        "^^^list_files\n",
        "project: test\n",
        "paths: [\n",
        "src\n",
        "]\n",
        "^^^\n\n",
        "Search for patterns:\n\n",
        "^^^search_files\n",
        "project: test\n",
        "regex: function.*\\(\n",
        "^^^\n\n",
        "But I shouldn't write yet:\n\n",
        "^^^write_file\n",
        "project: test\n",
        "path: test.txt\n",
        "content: test\n",
        "^^^"
    );

    let registry = crate::tools::test_registry();
    let filter = SmartToolFilter::new(&registry);
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should get the first three read tools but not the write tool
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[1].name, "list_files");
    assert_eq!(tools[2].name, "search_files");

    // Text should be truncated before the write_file tool
    assert!(truncated_text.contains("^^^search_files"));
    assert!(!truncated_text.contains("write_file"));
}

#[tokio::test]
async fn test_caret_parsing_with_smart_filter_blocks_write_after_read() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SmartToolFilter;

    let text = concat!(
        "First I'll read a file:\n\n",
        "^^^read_files\n",
        "project: test\n",
        "paths: [\n",
        "config.json\n",
        "]\n",
        "^^^\n\n",
        "Now I want to modify it:\n\n",
        "^^^replace_in_file\n",
        "project: test\n",
        "path: config.json\n",
        "diff ---\n",
        "some diff content\n",
        "--- diff\n",
        "^^^"
    );

    let registry = crate::tools::test_registry();
    let filter = SmartToolFilter::new(&registry);
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should only get the read tool, not the replace tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");

    // Text should be truncated before the replace_in_file tool
    assert!(truncated_text.contains("^^^read_files"));
    assert!(!truncated_text.contains("replace_in_file"));
}

#[tokio::test]
async fn test_caret_parsing_with_unlimited_filter_allows_all_tools() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::UnlimitedToolFilter;

    let text = concat!(
        "I'll do multiple operations:\n\n",
        "^^^read_files\n",
        "project: test\n",
        "paths: [\n",
        "file1.txt\n",
        "]\n",
        "^^^\n\n",
        "^^^write_file\n",
        "project: test\n",
        "path: file2.txt\n",
        "content: new content\n",
        "^^^\n\n",
        "^^^execute_command\n",
        "project: test\n",
        "command_line: ls -la\n",
        "^^^"
    );

    let filter = UnlimitedToolFilter;
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should get all three tools
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[1].name, "write_file");
    assert_eq!(tools[2].name, "execute_command");

    // Text should contain all tool blocks
    assert!(truncated_text.contains("^^^read_files"));
    assert!(truncated_text.contains("^^^write_file"));
    assert!(truncated_text.contains("^^^execute_command"));
}

#[tokio::test]
async fn test_caret_parsing_with_truncation_preserves_valid_tool() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    let text = concat!(
        "I will analyze the project structure:\n\n",
        "^^^read_files\n",
        "project: first-project\n",
        "paths: [\n",
        "package.json\n",
        "]\n",
        "^^^\n\n",
        "^^^read_files\n",
        "project: incomplete-"
    );

    let filter = SingleToolFilter;
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should succeed and return the valid first tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[0].input["project"], "first-project");

    let paths = tools[0].input["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "package.json");

    // Truncated text should end after the first tool block and not contain the incomplete second tool
    assert!(truncated_text.contains("^^^read_files"));
    assert!(truncated_text.contains("^^^"));

    // The text should end cleanly after first tool block
    assert!(
        truncated_text.trim_end().ends_with("^^^"),
        "Text should end cleanly after first tool block"
    );
}

#[tokio::test]
async fn test_caret_parsing_multiline_parameters_with_filter() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    let text = concat!(
        "I'll write a file with multiline content:\n\n",
        "^^^write_file\n",
        "project: test\n",
        "path: test.txt\n",
        "content ---\n",
        "This is line 1\n",
        "This is line 2\n",
        "This is line 3\n",
        "--- content\n",
        "^^^\n\n",
        "Then execute a command:\n\n",
        "^^^execute_command\n",
        "project: test\n",
        "command_line ---\n",
        "echo 'hello world'\n",
        "--- command_line\n",
        "^^^"
    );

    let filter = SingleToolFilter;
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should only get the first tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "write_file");
    assert_eq!(tools[0].input["project"], "test");
    assert_eq!(tools[0].input["path"], "test.txt");

    let content = tools[0].input["content"].as_str().unwrap();
    assert!(content.contains("This is line 1"));
    assert!(content.contains("This is line 2"));
    assert!(content.contains("This is line 3"));

    // Text should be truncated after the first tool
    assert!(truncated_text.contains("^^^write_file"));
    assert!(!truncated_text.contains("execute_command"));
}

#[tokio::test]
async fn test_caret_parsing_edge_case_empty_arrays_with_filter() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    let text = concat!(
        "Testing with empty arrays:\n\n",
        "^^^list_files\n",
        "project: test\n",
        "paths: [\n",
        "]\n",
        "^^^\n\n",
        "^^^read_files\n",
        "project: test\n",
        "paths: [\n",
        "file1.txt\n",
        "]\n",
        "^^^"
    );

    let filter = SingleToolFilter;
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter), &crate::tools::test_registry()).unwrap();

    // Should only get the first tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "list_files");
    assert_eq!(tools[0].input["project"], "test");

    // Empty array should be preserved
    let paths = tools[0].input["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 0);

    // Text should be truncated after the first tool
    assert!(truncated_text.contains("^^^list_files"));
    assert!(!truncated_text.contains("read_files"));
}

#[test]
fn test_caret_array_parsing() -> Result<()> {

    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "docs/customizable-tool-syntax.md\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0, &crate::tools::test_registry())?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "read_files");
    assert_eq!(
        tool_requests[0]
            .input
            .get("project")
            .unwrap()
            .as_str()
            .unwrap(),
        "code-assistant"
    );

    // This should be an array, not a string
    let paths = tool_requests[0].input.get("paths").unwrap();
    println!("paths value: {paths:?}");
    println!("paths type: {paths:?}");

    if paths.is_array() {
        let paths_array = paths.as_array().unwrap();
        assert_eq!(paths_array.len(), 1);
        assert_eq!(paths_array[0], "docs/customizable-tool-syntax.md");
    } else {
        panic!("Expected paths to be an array, but got: {paths:?}");
    }

    Ok(())
}

#[test]
fn test_caret_empty_array_parsing() -> Result<()> {

    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0, &crate::tools::test_registry())?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "read_files");

    // Empty array should still be an array
    let paths = tool_requests[0].input.get("paths").unwrap();
    assert!(paths.is_array());
    assert_eq!(paths.as_array().unwrap().len(), 0);

    Ok(())
}

#[test]
fn test_caret_multiple_arrays_parsing() -> Result<()> {

    let text = concat!(
        "^^^search_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "src/\n",
        "docs/\n",
        "]\n",
        "regex: single-value\n",
        "extensions: [\n",
        "rs\n",
        "md\n",
        "toml\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0, &crate::tools::test_registry())?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "search_files");

    // Check single value parameter
    let regex = tool_requests[0].input.get("regex").unwrap();
    assert!(regex.is_string());
    assert_eq!(regex.as_str().unwrap(), "single-value");

    // Check first array parameter
    let paths = tool_requests[0].input.get("paths").unwrap();
    assert!(paths.is_array());
    let paths_array = paths.as_array().unwrap();
    assert_eq!(paths_array.len(), 2);
    assert_eq!(paths_array[0], "src/");
    assert_eq!(paths_array[1], "docs/");

    // Check second array parameter
    let extensions = tool_requests[0].input.get("extensions").unwrap();
    assert!(extensions.is_array());
    let ext_array = extensions.as_array().unwrap();
    assert_eq!(ext_array.len(), 3);
    assert_eq!(ext_array[0], "rs");
    assert_eq!(ext_array[1], "md");
    assert_eq!(ext_array[2], "toml");

    Ok(())
}

#[test]
fn test_caret_array_with_multiline_parsing() -> Result<()> {

    let text = concat!(
        "^^^write_file\n",
        "project: code-assistant\n",
        "path: test.txt\n",
        "tags: [\n",
        "important\n",
        "test-file\n",
        "]\n",
        "content ---\n",
        "This is the file content\n",
        "with multiple lines\n",
        "--- content\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0, &crate::tools::test_registry())?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "write_file");

    // Check single parameters
    assert_eq!(
        tool_requests[0]
            .input
            .get("project")
            .unwrap()
            .as_str()
            .unwrap(),
        "code-assistant"
    );
    assert_eq!(
        tool_requests[0]
            .input
            .get("path")
            .unwrap()
            .as_str()
            .unwrap(),
        "test.txt"
    );

    // Check array parameter
    let tags = tool_requests[0].input.get("tags").unwrap();
    assert!(tags.is_array());
    let tags_array = tags.as_array().unwrap();
    assert_eq!(tags_array.len(), 2);
    assert_eq!(tags_array[0], "important");
    assert_eq!(tags_array[1], "test-file");

    // Check multiline parameter
    let content = tool_requests[0].input.get("content").unwrap();
    assert!(content.is_string());
    assert_eq!(
        content.as_str().unwrap(),
        "This is the file content\nwith multiple lines"
    );

    Ok(())
}

#[test]
fn test_original_caret_issue_reproduction() -> Result<()> {

    // This is the exact block that was reported as failing
    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "docs/customizable-tool-syntax.md\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    let result = parser.extract_requests(&response, 123, 0, &crate::tools::test_registry());

    match result {
        Ok((tool_requests, _truncated_response)) => {
            assert_eq!(tool_requests.len(), 1);
            assert_eq!(tool_requests[0].name, "read_files");
            assert_eq!(
                tool_requests[0]
                    .input
                    .get("project")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "code-assistant"
            );

            // This was the original issue - paths should be parsed as an array, not a string
            let paths = tool_requests[0].input.get("paths").unwrap();

            // Before the fix, this would fail with: "invalid type: string, expected a sequence"
            // Now it should work correctly
            assert!(paths.is_array(), "paths should be an array, not a string");
            let paths_array = paths.as_array().unwrap();
            assert_eq!(paths_array.len(), 1);
            assert_eq!(paths_array[0], "docs/customizable-tool-syntax.md");

            println!("✅ Original issue has been fixed!");
            println!("   paths parsed as: {paths:?}");
        }
        Err(e) => {
            panic!("Parser should not fail anymore, but got error: {e}");
        }
    }

    Ok(())
}
