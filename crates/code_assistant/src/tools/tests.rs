use crate::tests::mocks::ToolTestFixture;
use crate::tools::core::{ResourcesTracker, ToolRegistry};
use crate::tools::impls::{ListFilesTool, ListProjectsTool, ReadFilesTool, WriteFileTool};

use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;

use crate::tools::impls::{ExecuteCommandTool, ReplaceInFileTool};
use command_executor::CommandOutput;

#[tokio::test]
async fn test_read_files_tool() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register just the read files tool
    registry.register(Box::new(ReadFilesTool));

    // Create test fixture with sample files
    let mut fixture = ToolTestFixture::with_files(vec![(
        "test.txt".to_string(),
        "line 1\nline 2\nline 3\n".to_string(),
    )]);
    let mut context = fixture.context();

    // Test read_files tool - full file
    {
        // Get the tool from the registry
        let read_files_tool = registry
            .get("read_files")
            .expect("read_files tool should be registered");

        // Parameters for read_files
        let mut params = json!({
            "project": "test-project",
            "paths": ["test.txt"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully loaded"));
        assert!(output.contains(">>>>> FILE: test.txt"));
        assert!(output.contains("line 1"));
        assert!(output.contains("line 2"));
        assert!(output.contains("line 3"));
    }

    // Test read_files tool - with line range
    {
        // Get the tool from the registry
        let read_files_tool = registry
            .get("read_files")
            .expect("read_files tool should be registered");

        // Parameters for read_files with line range
        let mut params = json!({
            "project": "test-project",
            "paths": ["test.txt:2-3"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully loaded"));
        // The important parts are the filename and the content
        assert!(output.contains("test.txt"));
        assert!(!output.contains("line 1"));
        assert!(output.contains("line 2"));
        assert!(output.contains("line 3"));
    }

    Ok(())
}

#[tokio::test]
async fn test_write_file_tool() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register just the write file tool
    registry.register(Box::new(WriteFileTool));

    // Create test fixture with existing file
    let mut fixture = ToolTestFixture::with_files(vec![(
        "existing.txt".to_string(),
        "original content".to_string(),
    )]);
    let mut context = fixture.context();

    // Test write_file tool - create new file
    {
        // Get the tool from the registry
        let write_file_tool = registry
            .get("write_file")
            .expect("write_file tool should be registered");

        // Parameters for write_file
        let mut params = json!({
            "project": "test-project",
            "path": "new_file.txt",
            "content": "This is new content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully wrote"));
        assert!(output.contains("new_file.txt"));
    }

    // Test write_file tool - overwrite existing file
    {
        // Get the tool from the registry
        let write_file_tool = registry
            .get("write_file")
            .expect("write_file tool should be registered");

        // Parameters for write_file
        let mut params = json!({
            "project": "test-project",
            "path": "existing.txt",
            "content": "This is replacement content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully wrote"));
        assert!(output.contains("existing.txt"));
    }

    // Test write_file tool - append to file
    {
        // Get the tool from the registry
        let write_file_tool = registry
            .get("write_file")
            .expect("write_file tool should be registered");

        // Parameters for write_file
        let mut params = json!({
            "project": "test-project",
            "path": "existing.txt",
            "content": "\nAppended content",
            "append": true
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Success"));
        assert!(output.contains("existing.txt"));

        // Get the updated content from the explorer to verify it was appended
        let explorer = context
            .project_manager
            .get_explorer_for_project("test-project")?;
        let updated_content = explorer
            .read_file(&PathBuf::from("./root/existing.txt"))
            .await?;
        assert!(updated_content.contains("This is replacement content\nAppended content"));
    }

    Ok(())
}

#[tokio::test]
async fn test_replace_in_file_tool() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register just the replace in file tool
    registry.register(Box::new(ReplaceInFileTool));

    // Set up sample test files
    let source_file_content = concat!(
        "function test() {\n",
        "  console.log(\"old message\");\n",
        "  const x=42;\n",
        "  return x;\n",
        "}"
    );

    let mut fixture = ToolTestFixture::with_files(vec![(
        "source.txt".to_string(),
        source_file_content.to_string(),
    )]);
    let mut context = fixture.context();

    // Test replace_in_file tool
    {
        // Get the tool from the registry
        let replace_in_file_tool = registry
            .get("replace_in_file")
            .expect("replace_in_file tool should be registered");

        // Create diff content using the search-replace format
        let diff = concat!(
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "  console.log(\"old message\");\n",
            "=======\n",
            "function test() {\n",
            "  console.log(\"new message\");\n",
            ">>>>>>> REPLACE\n",
            "\n",
            "<<<<<<< SEARCH\n",
            "  const x=42;\n",
            "=======\n",
            "  const x = 42;\n",
            ">>>>>>> REPLACE"
        );

        // Parameters for replace_in_file
        let mut params = json!({
            "project": "test-project",
            "path": "source.txt",
            "diff": diff
        });

        // Execute the tool
        let result = replace_in_file_tool
            .invoke(&mut context, &mut params)
            .await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully replaced"));

        // Verify the content was actually replaced by reading the file
        let explorer = context
            .project_manager
            .get_explorer_for_project("test-project")?;
        let updated_content = explorer
            .read_file(&PathBuf::from("./root/source.txt"))
            .await?;

        // Verify both replacements were made
        assert!(updated_content.contains("console.log(\"new message\")"));
        assert!(updated_content.contains("const x = 42"));
    }

    // Test error case - text not found
    {
        // Get the tool from the registry
        let replace_in_file_tool = registry
            .get("replace_in_file")
            .expect("replace_in_file tool should be registered");

        // Create diff with content that doesn't match
        let diff = concat!(
            "<<<<<<< SEARCH\n",
            "This text doesn't exist in the file\n",
            "=======\n",
            "Replacement text\n",
            ">>>>>>> REPLACE\n"
        );

        // Parameters for replace_in_file
        let mut params = json!({
            "project": "test-project",
            "path": "source.txt",
            "diff": diff
        });

        // Execute the tool
        let result = replace_in_file_tool
            .invoke(&mut context, &mut params)
            .await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output indicates an error
        assert!(!output.contains("Successfully replaced"));
        assert!(output.contains("SEARCH block"));
    }

    Ok(())
}

#[tokio::test]
async fn test_execute_command_tool() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register just the execute command tool
    registry.register(Box::new(ExecuteCommandTool));

    // Create test fixture with command response
    let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
        success: true,
        output: "Command output\nLine 2\nWarning message".to_string(),
    })]);
    let mut context = fixture.context();

    // Test execute_command tool - successful command
    {
        // Get the tool from the registry
        let execute_command_tool = registry
            .get("execute_command")
            .expect("execute_command tool should be registered");

        // Parameters for execute_command
        let mut params = json!({
            "project": "test",
            "command_line": "ls -la",
            "working_dir": "src"
        });

        // Execute the tool
        let result = execute_command_tool
            .invoke(&mut context, &mut params)
            .await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Status: Success"));
        assert!(output.contains("Command output"));
        assert!(output.contains("Warning message"));
    }

    // Now test with a failing command
    {
        // Create new fixture with failing command
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: false,
            output: "Some output\nError: command failed".to_string(),
        })]);
        let mut context = fixture.context();

        // Get the tool from the registry
        let execute_command_tool = registry
            .get("execute_command")
            .expect("execute_command tool should be registered");

        // Parameters for execute_command
        let mut params = json!({
            "project": "test",
            "command_line": "invalid-command"
        });

        // Execute the tool
        let result = execute_command_tool
            .invoke(&mut context, &mut params)
            .await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Status: Failed"));
        assert!(output.contains("Error: command failed"));
    }

    Ok(())
}

#[tokio::test]
async fn test_tool_dispatch_via_registry() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register tools manually rather than using the global registry
    registry.register(Box::new(ListFilesTool));
    registry.register(Box::new(ListProjectsTool));
    registry.register(Box::new(ReadFilesTool));
    registry.register(Box::new(WriteFileTool));

    // Create test fixture with test file
    let mut fixture = ToolTestFixture::with_files(vec![(
        "test.txt".to_string(),
        "Test file content".to_string(),
    )]);
    let mut context = fixture.context();

    // Test list_projects tool
    {
        // Get the tool from the registry
        let list_projects_tool = registry
            .get("list_projects")
            .expect("list_projects tool should be registered");

        // Parameters for list_projects (empty object)
        let mut params = json!({});

        // Execute the tool
        let result = list_projects_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = crate::tools::core::ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Available projects:"));
        assert!(output.contains("- test-project"));
    }

    // Test read_files tool
    {
        // Get the tool from the registry
        let read_files_tool = registry
            .get("read_files")
            .expect("read_files tool should be registered");

        // Parameters for read_files
        let mut params = json!({
            "project": "test-project",
            "paths": ["test.txt", "test.txt:10-20"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = crate::tools::core::ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully loaded"));
        assert!(output.contains(">>>>> FILE: test.txt"));
    }

    // Test write_file tool
    {
        // Get the tool from the registry
        let write_file_tool = registry
            .get("write_file")
            .expect("write_file tool should be registered");

        // Parameters for write_file
        let mut params = json!({
            "project": "test-project",
            "path": "new_test.txt",
            "content": "This is a test content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = crate::tools::core::ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully wrote"));
        assert!(output.contains("new_test.txt"));
    }

    // Test list_files tool
    {
        // Get the tool from the registry
        let list_files_tool = registry
            .get("list_files")
            .expect("list_files tool should be registered");

        // Parameters for list_files
        let mut params = json!({
            "project": "test-project",
            "paths": ["."],
            "max_depth": 2
        });

        // Execute the tool
        let result = list_files_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = crate::tools::core::ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // The output should contain information about our test file
        assert!(output.contains("Path: ."));
        assert!(output.contains("test.txt"));
    }

    Ok(())
}

#[test]
fn test_tool_use_docs_generation() {
    use crate::agent::ToolSyntax;
    use crate::tools::core::ToolScope;
    use crate::tools::ParserRegistry;

    // Test XML documentation
    let xml_parser = ParserRegistry::get(ToolSyntax::Xml);
    if let Some(xml_docs) = xml_parser.generate_tool_documentation(ToolScope::Agent) {
        println!("=== XML Tool Documentation ===");
        println!("{}", &xml_docs[..1500.min(xml_docs.len())]);
        println!("...\n");
    }

    // Test Caret documentation
    let caret_parser = ParserRegistry::get(ToolSyntax::Caret);
    if let Some(caret_docs) = caret_parser.generate_tool_documentation(ToolScope::Agent) {
        println!("=== Caret Tool Documentation ===");
        println!("{}", &caret_docs[..1500.min(caret_docs.len())]);
        println!("...\n");
    }

    // Test Native documentation (should be None)
    let native_parser = ParserRegistry::get(ToolSyntax::Native);
    if native_parser
        .generate_tool_documentation(ToolScope::Agent)
        .is_some()
    {
        println!("Native parser unexpectedly returned documentation");
    } else {
        println!("=== Native Tool Documentation ===");
        println!("None (as expected - uses API tool definitions)\n");
    }

    // Test syntax documentation
    println!("=== XML Syntax Documentation ===");
    if let Some(xml_syntax) = xml_parser.generate_syntax_documentation() {
        println!("{}", &xml_syntax[..1500.min(xml_syntax.len())]);
        println!("...\n");
    }

    println!("=== Caret Syntax Documentation ===");
    if let Some(caret_syntax) = caret_parser.generate_syntax_documentation() {
        println!("{}", &caret_syntax[..1500.min(caret_syntax.len())]);
        println!("...\n");
    }

    println!("=== Native Syntax Documentation ===");
    if native_parser.generate_syntax_documentation().is_some() {
        println!("Native parser unexpectedly returned syntax documentation");
    } else {
        println!("None (as expected - uses native function calls)\n");
    }
}

#[test]
fn test_parameter_documentation_formatting() {
    use crate::agent::ToolSyntax;
    use crate::tools::ParserRegistry;
    use serde_json::json;

    // Test parameter formatting with XML parser
    let xml_parser = ParserRegistry::get(ToolSyntax::Xml);

    // Test simple parameter documentation
    let _param = json!({
        "type": "string",
        "description": "Path to the file"
    });

    // The parser methods are private, so we test via full tool documentation generation
    // and check that our formatting logic works correctly by testing specific tools

    // Test parameter with required marking in description
    let _required_param = json!({
        "type": "string",
        "description": "Project name (required)"
    });

    // Test that array parameters work correctly by checking generated docs contain proper examples
    if let Some(docs) = xml_parser.generate_tool_documentation(crate::tools::core::ToolScope::Agent)
    {
        // Should contain XML-style parameter examples
        assert!(
            docs.contains("<param:"),
            "XML docs should contain parameter examples"
        );
        assert!(
            docs.contains("</param:"),
            "XML docs should contain parameter closing tags"
        );

        // Should contain required parameter markers
        assert!(
            docs.contains("(required)"),
            "XML docs should mark required parameters"
        );

        // Should handle multiline parameters properly
        assert!(
            docs.contains("command_line"),
            "XML docs should include multiline parameter examples"
        );

        println!("✓ XML parameter documentation formatting verified");
    }

    // Test with Caret parser
    let caret_parser = ParserRegistry::get(ToolSyntax::Caret);
    if let Some(docs) =
        caret_parser.generate_tool_documentation(crate::tools::core::ToolScope::Agent)
    {
        // Should contain caret-style parameter examples
        assert!(
            docs.contains("^^^"),
            "Caret docs should contain caret block examples"
        );
        assert!(
            docs.contains(": "),
            "Caret docs should contain key: value parameter examples"
        );

        // Should contain required parameter markers
        assert!(
            docs.contains("(required)"),
            "Caret docs should mark required parameters"
        );

        // Should handle array parameters properly
        assert!(
            docs.contains("["),
            "Caret docs should include array parameter examples"
        );
        assert!(
            docs.contains("]"),
            "Caret docs should include array parameter examples"
        );

        // Should handle multiline parameters properly
        assert!(
            docs.contains("---"),
            "Caret docs should include multiline parameter examples"
        );

        println!("✓ Caret parameter documentation formatting verified");
    }
}

#[test]
fn test_usage_example_generation() {
    use crate::agent::ToolSyntax;
    use crate::tools::ParserRegistry;

    // Test both XML and Caret parsers generate proper usage examples
    let xml_parser = ParserRegistry::get(ToolSyntax::Xml);
    let caret_parser = ParserRegistry::get(ToolSyntax::Caret);

    if let Some(xml_docs) =
        xml_parser.generate_tool_documentation(crate::tools::core::ToolScope::Agent)
    {
        // XML usage examples should be properly formatted
        assert!(
            xml_docs.contains("<tool:"),
            "XML docs should contain tool examples"
        );
        assert!(
            xml_docs.contains("</tool:"),
            "XML docs should contain tool closing tags"
        );

        // Should handle array parameters correctly (paths -> path singular form)
        if xml_docs.contains("paths") {
            assert!(
                xml_docs.contains("<param:path>"),
                "XML docs should use singular form for array parameters"
            );
        }

        // Should contain realistic parameter placeholders
        assert!(
            xml_docs.contains("project-name") || xml_docs.contains("File path here"),
            "XML docs should contain realistic placeholders"
        );

        println!("✓ XML usage example generation verified");
    }

    if let Some(caret_docs) =
        caret_parser.generate_tool_documentation(crate::tools::core::ToolScope::Agent)
    {
        // Caret usage examples should be properly formatted
        assert!(
            caret_docs.contains("^^^"),
            "Caret docs should contain caret block examples"
        );

        // Should handle multiline parameters correctly
        if caret_docs.contains("command_line") {
            assert!(
                caret_docs.contains("command_line ---"),
                "Caret docs should show multiline parameter syntax"
            );
            assert!(
                caret_docs.contains("--- command_line"),
                "Caret docs should show multiline parameter closing"
            );
        }

        // Should handle array parameters correctly
        if caret_docs.contains("paths") {
            assert!(
                caret_docs.contains("paths: ["),
                "Caret docs should show array parameter syntax"
            );
        }

        // Should contain realistic parameter placeholders
        assert!(
            caret_docs.contains("project-name") || caret_docs.contains("File path here"),
            "Caret docs should contain realistic placeholders"
        );

        println!("✓ Caret usage example generation verified");
    }
}

#[tokio::test]
async fn test_xml_parsing_fails_with_valid_first_block_and_invalid_second() {
    use crate::tools::parse::parse_xml_tool_invocations;

    // This reproduces the exact error scenario described by the user
    let text = r#"I will analyze the structure and code of the project to find out which tool-use syntax is used. Let me start with the most important files.

<tool:read_files>
<param:project>qwen-code</param:project>
<param:path>package.json</param:path>
</tool:read_files>

<tool:read_files>
<param:project>qwen-"#;

    // Currently this fails completely, losing the valid first tool block
    let result = parse_xml_tool_invocations(text, 123, 0, None);

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
    use crate::tools::parse::parse_xml_tool_invocations;
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
        parse_xml_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_xml_tool_invocations;
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
    let result = parse_xml_tool_invocations(text, 123, 0, Some(&filter));

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
    use crate::tools::parse::parse_xml_tool_invocations;
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
    let (tools, truncated_text) = parse_xml_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_xml_tool_invocations;
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
    let (tools, truncated_text) = parse_xml_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

    // Should only get the read tool, not the replace tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");

    // Text should be truncated before the replace_in_file tool
    assert!(truncated_text.contains("</tool:read_files>"));
    assert!(!truncated_text.contains("<tool:replace_in_file>"));
}

#[tokio::test]
async fn test_xml_parsing_with_unlimited_filter_allows_all_tools() {
    use crate::tools::parse::parse_xml_tool_invocations;
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
    let (tools, truncated_text) = parse_xml_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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

#[tokio::test]
async fn test_caret_parsing_with_single_tool_filter() {
    use crate::tools::parse::parse_caret_tool_invocations;
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
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_caret_tool_invocations;
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

    let filter = SmartToolFilter::new();
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_caret_tool_invocations;
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

    let filter = SmartToolFilter::new();
    let (tools, truncated_text) =
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

    // Should only get the read tool, not the replace tool
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_files");

    // Text should be truncated before the replace_in_file tool
    assert!(truncated_text.contains("^^^read_files"));
    assert!(!truncated_text.contains("replace_in_file"));
}

#[tokio::test]
async fn test_caret_parsing_with_unlimited_filter_allows_all_tools() {
    use crate::tools::parse::parse_caret_tool_invocations;
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
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_caret_tool_invocations;
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
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_caret_tool_invocations;
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
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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
    use crate::tools::parse::parse_caret_tool_invocations;
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
        parse_caret_tool_invocations(text, 123, 0, Some(&filter)).unwrap();

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

#[tokio::test]
async fn test_mixed_syntax_scenarios_with_filters() {
    use crate::tools::parse::{parse_caret_tool_invocations, parse_xml_tool_invocations};
    use crate::tools::tool_use_filter::{SingleToolFilter, SmartToolFilter};

    // Test that each parser handles its own syntax correctly with filters

    // XML with web tools (read operations)
    let xml_text = r#"Let me fetch some web content:

<tool:web_fetch>
<param:url>https://example.com</param:url>
</tool:web_fetch>

<tool:web_search>
<param:query>rust programming</param:query>
<param:hits_page_number>1</param:hits_page_number>
</tool:web_search>"#;

    let smart_filter = SmartToolFilter::new();
    let (xml_tools, _) = parse_xml_tool_invocations(xml_text, 123, 0, Some(&smart_filter)).unwrap();

    // Should get both web tools (they're read operations)
    assert_eq!(xml_tools.len(), 2);
    assert_eq!(xml_tools[0].name, "web_fetch");
    assert_eq!(xml_tools[1].name, "web_search");

    // Caret with the same tools
    let caret_text = concat!(
        "Let me fetch some web content:\n\n",
        "^^^web_fetch\n",
        "url: https://example.com\n",
        "^^^\n\n",
        "^^^web_search\n",
        "query: rust programming\n",
        "hits_page_number: 1\n",
        "^^^"
    );

    let (caret_tools, _) =
        parse_caret_tool_invocations(caret_text, 123, 0, Some(&smart_filter)).unwrap();

    // Should get both web tools (they're read operations)
    assert_eq!(caret_tools.len(), 2);
    assert_eq!(caret_tools[0].name, "web_fetch");
    assert_eq!(caret_tools[1].name, "web_search");

    // Test with SingleToolFilter - should only get first tool in both cases
    let single_filter = SingleToolFilter;

    let (xml_single, _) =
        parse_xml_tool_invocations(xml_text, 123, 0, Some(&single_filter)).unwrap();
    assert_eq!(xml_single.len(), 1);
    assert_eq!(xml_single[0].name, "web_fetch");

    let (caret_single, _) =
        parse_caret_tool_invocations(caret_text, 123, 0, Some(&single_filter)).unwrap();
    assert_eq!(caret_single.len(), 1);
    assert_eq!(caret_single[0].name, "web_fetch");
}
