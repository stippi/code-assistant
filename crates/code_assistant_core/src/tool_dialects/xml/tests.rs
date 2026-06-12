//! XML dialect tests: invocation parsing with filters and truncation, and
//! the parser behaviors formerly tested through the agent test module.

use crate::tools::ToolRequest;
use crate::types::ToolSyntax;
use anyhow::Result;
use llm::{ContentBlock, LLMResponse, Usage};

/// Parse tool requests from an LLM response with this dialect and return
/// both requests and the truncated response after the first tool.
fn parse_and_truncate_llm_response(
    response: &LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    crate::tool_dialects::dialect_for(ToolSyntax::Xml).extract_requests(response, request_id, 0)
}

#[tokio::test]
async fn test_xml_parsing_fails_with_valid_first_block_and_invalid_second() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;

    // This reproduces the exact error scenario described by the user
    let text = r#"I will analyze the structure and code of the project to find out which tool-use syntax is used. Let me start with the most important files.

<tool:read_files>
<param:project>qwen-code</param:project>
<param:path>package.json</param:path>
</tool:read_files>

<tool:read_files>
<param:project>qwen-"#;

    // Currently this fails completely, losing the valid first tool block
    let result = parse_xml_tool_invocations(text, 123, 0, None, crate::tools::global_registry());

    // This assertion will currently fail because the function returns an error
    // instead of returning the valid first tool block
    assert!(
        result.is_err(),
        "Expected parsing to fail with current implementation"
    );

    // The error should be about the incomplete second tool block
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("unclosed") || error_msg.contains("Malformed"));
}

#[tokio::test]
async fn test_xml_parsing_with_multiple_valid_blocks_should_limit_to_first() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    // Test case where we have multiple valid blocks but want to limit to first
    let text = r#"Let me read these files:

<tool:read_files>
<param:project>test</param:project>
<param:path>file1.txt</param:path>
</tool:read_files>

And then list the directory:

<tool:list_files>
<param:project>test</param:project>
<param:paths>src</param:paths>
</tool:list_files>

And finally modify the file:

<tool:replace_in_file>
<param:project>test</param:project>
<param:path>file1.txt</param:path>
<param:diff>some diff</param:diff>
</tool:replace_in_file>"#;

    // With the new system, this should return only the first tool and truncate after it
    let filter = SingleToolFilter;
    let (result, _truncated_text) =
        parse_xml_tool_invocations(text, 123, 0, Some(&filter), crate::tools::global_registry()).unwrap();

    // Should only get the first tool
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "read_files");
    assert_eq!(result[0].input["project"], "test");

    // The XML parser converts single path parameters to the paths array based on the schema
    let paths = result[0].input["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "file1.txt");
}

#[tokio::test]
async fn test_xml_parsing_with_truncation_preserves_valid_tool() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::SingleToolFilter;

    // Test that truncation function can extract valid tool even with invalid following content
    let text = r#"I will analyze the structure and code of the project.

<tool:read_files>
<param:project>first-project</param:project>
<param:path>package.json</param:path>
</tool:read_files>

<tool:read_files>
<param:project>incomplete-"#;

    let filter = SingleToolFilter;
    let result = parse_xml_tool_invocations(text, 123, 0, Some(&filter), crate::tools::global_registry());

    // Should succeed and return the valid first tool
    assert!(
        result.is_ok(),
        "Truncation should succeed and preserve valid tool"
    );
    let (tools, truncated_text) = result.unwrap();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[0].input["project"], "first-project");

    // The XML parser converts single path parameters to the paths array based on the schema
    let paths = tools[0].input["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "package.json");

    // Truncated text should end after the first tool block and not contain the incomplete second tool
    assert!(truncated_text.contains("</tool:read_files>"));

    // The text should end with the closing tag of the first tool, no incomplete second tool
    assert!(
        truncated_text.trim_end().ends_with("</tool:read_files>"),
        "Text should end cleanly after first tool block"
    );
}

#[tokio::test]
async fn test_xml_parsing_with_smart_filter_allows_multiple_read_tools() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::SmartToolFilter;

    let text = r#"Let me analyze the project structure:

<tool:read_files>
<param:project>test</param:project>
<param:path>package.json</param:path>
</tool:read_files>

Now let me list the directories:

<tool:list_files>
<param:project>test</param:project>
<param:paths>src</param:paths>
</tool:list_files>

Let me search for specific patterns:

<tool:search_files>
<param:project>test</param:project>
<param:regex>function.*\(</param:regex>
</tool:search_files>

But I shouldn't be able to write files yet:

<tool:write_file>
<param:project>test</param:project>
<param:path>test.txt</param:path>
<param:content>test content</param:content>
</tool:write_file>"#;

    let filter = SmartToolFilter::new();
    let (tools, truncated_text) =
        parse_xml_tool_invocations(text, 123, 0, Some(&filter), crate::tools::global_registry()).unwrap();

    // Should get the first three read tools but not the write tool
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[1].name, "list_files");
    assert_eq!(tools[2].name, "search_files");

    // Text should be truncated after the search_files block
    assert!(truncated_text.ends_with("</tool:search_files>"));
    assert!(!truncated_text.contains("<tool:write_file>"));
}

#[tokio::test]
async fn test_xml_parsing_with_smart_filter_blocks_write_after_read() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::SmartToolFilter;

    let text = r#"First I'll read a file:

<tool:read_files>
<param:project>test</param:project>
<param:path>config.json</param:path>
</tool:read_files>

Now I want to modify it immediately:

<tool:replace_in_file>
<param:project>test</param:project>
<param:path>config.json</param:path>
<param:diff>some diff content</param:diff>
</tool:replace_in_file>"#;

    let filter = SmartToolFilter::new();
    let (tools, truncated_text) =
        parse_xml_tool_invocations(text, 123, 0, Some(&filter), crate::tools::global_registry()).unwrap();

    // Should only get the read tool, not the replace tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");

    // Text should be truncated before the replace_in_file tool
    assert!(truncated_text.contains("</tool:read_files>"));
    assert!(!truncated_text.contains("<tool:replace_in_file>"));
}

#[tokio::test]
async fn test_xml_parsing_with_unlimited_filter_allows_all_tools() {
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::UnlimitedToolFilter;

    let text = r#"I'll do multiple operations:

<tool:read_files>
<param:project>test</param:project>
<param:path>file1.txt</param:path>
</tool:read_files>

<tool:write_file>
<param:project>test</param:project>
<param:path>file2.txt</param:path>
<param:content>new content</param:content>
</tool:write_file>

<tool:execute_command>
<param:project>test</param:project>
<param:command_line>ls -la</param:command_line>
</tool:execute_command>"#;

    let filter = UnlimitedToolFilter;
    let (tools, truncated_text) =
        parse_xml_tool_invocations(text, 123, 0, Some(&filter), crate::tools::global_registry()).unwrap();

    // Should get all three tools
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "read_files");
    assert_eq!(tools[1].name, "write_file");
    assert_eq!(tools[2].name, "execute_command");

    // Text should contain all tool blocks
    assert!(truncated_text.contains("</tool:read_files>"));
    assert!(truncated_text.contains("</tool:write_file>"));
    assert!(truncated_text.contains("</tool:execute_command>"));
}

#[test]
fn test_flexible_xml_parsing() -> Result<()> {
    let text = concat!(
        "I will search for TODO comments in the code.\n",
        "\n",
        "<tool:search_files>\n",
        "<param:project>test</param:project>\n",
        "<param:regex>TODO & FIXME <html></param:regex>\n",
        "</tool:search_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    // Use a test request_id
    let request_id = 42;

    let (tool_requests, _truncated_response) =
        parse_and_truncate_llm_response(&response, request_id)?;
    assert_eq!(tool_requests.len(), 1);

    let request = &tool_requests[0];
    assert_eq!(request.name, "search_files");
    if let Some(regex) = request.input.get("regex") {
        assert_eq!(regex.as_str().unwrap(), "TODO & FIXME <html>"); // Notice the & character is allowed and also tags
    } else {
        panic!("Missing regex parameter");
    }

    Ok(())
}

#[test]
fn test_replacement_xml_parsing() -> Result<()> {
    let text = concat!(
        "I will fix the code formatting.\n",
        "\n",
        "<tool:edit>\n",
        "<param:project>test</param:project>\n",
        "<param:path>src/main.rs</param:path>\n",
        "<param:old_text>function test(){\n",
        "  console.log(\"messy\");\n",
        "}</param:old_text>\n",
        "<param:new_text>function test() {\n",
        "    console.log(\"clean\");\n",
        "}</param:new_text>\n",
        "</tool:edit>\n",
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    // Use a test request_id
    let request_id = 42;
    let (tool_requests, _truncated_response) =
        parse_and_truncate_llm_response(&response, request_id)?;
    assert_eq!(tool_requests.len(), 1);

    let request = &tool_requests[0];
    assert_eq!(request.name, "edit");
    assert_eq!(
        request.input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        request.input.get("path").unwrap().as_str().unwrap(),
        "src/main.rs"
    );

    let old_text = request.input.get("old_text").unwrap().as_str().unwrap();
    assert!(old_text.contains("function test(){"));
    assert!(old_text.contains("console.log(\"messy\")"));

    let new_text = request.input.get("new_text").unwrap().as_str().unwrap();
    assert!(new_text.contains("function test() {"));
    assert!(new_text.contains("console.log(\"clean\")"));

    Ok(())
}

#[tokio::test]
async fn test_mixed_tool_start_end() -> Result<()> {
    let text = concat!(
        "Now I will take a look at the drop down implementation:\n",
        "\n",
        "<tool:read_files>\n",
        "<param:project>gpui-component</param:project>\n",
        "<param:path>crates/ui/src/dropdown.rs</param:path>\n",
        "<param:path>crates/ui/src/menu</param:path>\n",
        "</tool:list_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);
    println!("result: {result:?}");

    // This should return an error, not Ok([])
    assert!(
        result.is_err(),
        "Expected ParseError for mismatched tool names"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("mismatching tool names"),
            "Error should mention mismatching tool names: {error_msg}"
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention read_files: {error_msg}"
        );
        assert!(
            error_msg.contains("list_files"),
            "Error should mention list_files: {error_msg}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_missing_closing_param_tag() -> Result<()> {
    let text = concat!(
        "Let me examine the current parsing logic more closely and then fix it:\n",
        "\n",
        "<tool:replace_in_file>\n",
        "<param:project>code-assistant</param:project>\n",
        "<param:path>crates/llm/src/openai.rs</param:path>\n",
        "<param:diff>\n",
        "<<<<<<< SEARCH\n",
        "        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {\n",
        "            headers.get(name).and_then(|h| h.to_str().ok()).map(|s| {\n",
        "                // Parse OpenAI's duration format (e.g., \"1s\", \"6m0s\")\n",
        "                let mut seconds = 0u64;\n",
        "                let mut current_num = String::new();\n",
        "\n",
        "                for c in s.chars() {\n",
        "                    match c {\n",
        "                        '0'..='9' => current_num.push(c),\n",
        "                        'm' => {\n",
        "                            if let Ok(mins) = current_num.parse::<u64>() {\n",
        "                                seconds += mins * 60;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        's' => {\n",
        "                            if let Ok(secs) = current_num.parse::<u64>() {\n",
        "                                seconds += secs;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        _ => current_num.clear(),\n",
        "                    }\n",
        "                }\n",
        "                Duration::from_secs(seconds)\n",
        "            })\n",
        "        }\n",
        "=======\n",
        "        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {\n",
        "            headers.get(name).and_then(|h| h.to_str().ok()).map(|s| {\n",
        "                // Parse OpenAI's duration format (e.g., \"1s\", \"6m0s\", \"7.66s\", \"2m59.56s\")\n",
        "                let mut total_seconds = 0.0f64;\n",
        "                let mut current_num = String::new();\n",
        "                \n",
        "                for c in s.chars() {\n",
        "                    match c {\n",
        "                        '0'..='9' | '.' => current_num.push(c),\n",
        "                        'm' => {\n",
        "                            if let Ok(mins) = current_num.parse::<f64>() {\n",
        "                                total_seconds += mins * 60.0;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        's' => {\n",
        "                            if let Ok(secs) = current_num.parse::<f64>() {\n",
        "                                total_seconds += secs;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        _ => current_num.clear(),\n",
        "                    }\n",
        "                }\n",
        "                Duration::from_secs_f64(total_seconds)\n",
        "            })\n",
        "        }\n",
        ">>>>>>> REPLACE\n",
        "</tool:replace_in_file>\n",
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);
    println!("result: {result:?}");

    // This should return an error, not Ok([])
    assert!(
        result.is_err(),
        "Expected ParseError for missing </param:diff> close tag"
    );

    // if let Err(ref error) = result {
    //     let error_msg = error.to_string();
    //     assert!(
    //         error_msg.contains("</param:diff>"),
    //         "Error should mention missing closing tag: {}",
    //         error_msg
    //     );
    // }

    Ok(())
}

#[test]
fn test_ignore_non_tool_tags() -> Result<()> {
    let text = concat!(
        "I will work with some HTML code:\n",
        "\n",
        "<div>Some HTML content</div>\n",
        "<tool:read_files>\n",
        "<param:project>test</param:project>\n",
        "<param:path>index.html</param:path>\n",
        "</tool:read_files>\n",
        "<p>More HTML after the tool</p>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (result, _truncated_response) = parse_and_truncate_llm_response(&response, 1)?;

    // Should successfully parse the tool while ignoring HTML tags
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "read_files");
    assert_eq!(
        result[0].input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        result[0].input.get("paths").unwrap().as_array().unwrap()[0],
        "index.html"
    );

    Ok(())
}

#[test]
fn test_html_between_tool_tags_should_error() -> Result<()> {
    let text = concat!(
        "I will read files with some HTML mixed in:\n",
        "\n",
        "<tool:read_files>\n",
        "<div>This HTML should not be here</div>\n",
        "<param:project>test</param:project>\n",
        "<param:path>index.html</param:path>\n",
        "</tool:read_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);

    // This should be an error since HTML tags between tool tags (but outside parameters) make the structure unclear
    assert!(
        result.is_err(),
        "Expected ParseError for HTML tag inside tool block"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("unexpected tag"),
            "Error should mention unexpected tag: {error_msg}"
        );
        assert!(
            error_msg.contains("div"),
            "Error should mention the div tag: {error_msg}"
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention the tool name: {error_msg}"
        );
    }

    Ok(())
}

#[test]
fn test_html_inside_parameter_allowed() -> Result<()> {
    // The existing test_flexible_xml_parsing already covers HTML content inside parameters
    // We'll just verify that our validation doesn't break that case
    let text = concat!(
        "I will search for content with special characters:\n",
        "\n",
        "<tool:search_files>\n",
        "<param:project>test</param:project>\n",
        "<param:regex><div id=\"test\"></param:regex>\n",
        "</tool:search_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (result, _truncated_response) = parse_and_truncate_llm_response(&response, 1)?;

    // Should successfully parse - special characters inside parameter content are allowed
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "search_files");
    assert_eq!(
        result[0].input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        result[0].input.get("regex").unwrap().as_str().unwrap(),
        "<div id=\"test\">"
    );

    Ok(())
}
