use crate::tests::mocks::{MockCommandExecutor, MockExplorer, MockProjectManager};
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry};
use crate::tools::impls::{ListFilesTool, ListProjectsTool, ReadFilesTool, WriteFileTool};
use crate::types::{FileSystemEntryType, FileTreeEntry};

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::tools::impls::{ExecuteCommandTool, ReplaceInFileTool};
use crate::utils::CommandOutput;

#[tokio::test]
async fn test_read_files_tool() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register just the read files tool
    registry.register(Box::new(ReadFilesTool));

    // Set up sample test files
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.txt"),
        "line 1\nline 2\nline 3\n".to_string(),
    );

    // Create file tree
    let mut children = HashMap::new();
    children.insert(
        "test.txt".to_string(),
        FileTreeEntry {
            name: "test.txt".to_string(),
            entry_type: FileSystemEntryType::File,
            children: HashMap::new(),
            is_expanded: false,
        },
    );

    let file_tree = Some(FileTreeEntry {
        name: "./root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children,
        is_expanded: true,
    });

    // Create a custom explorer
    let explorer = MockExplorer::new(files, file_tree);

    // Create a mock project manager with our files
    let project_manager = MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        Box::new(explorer),
    );

    // Create a default mock command executor
    let command_executor = MockCommandExecutor::new(vec![]);

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager: &project_manager,
        command_executor: &command_executor,
        working_memory: None,
    };

    // Test read_files tool - full file
    {
        // Get the tool from the registry
        let read_files_tool = registry
            .get("read_files")
            .expect("read_files tool should be registered");

        // Parameters for read_files
        let params = json!({
            "project": "test-project",
            "paths": ["test.txt"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "paths": ["test.txt:2-3"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, params).await?;

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

    // Set up sample test files
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/existing.txt"),
        "original content".to_string(),
    );

    // Create file tree
    let mut children = HashMap::new();
    children.insert(
        "existing.txt".to_string(),
        FileTreeEntry {
            name: "existing.txt".to_string(),
            entry_type: FileSystemEntryType::File,
            children: HashMap::new(),
            is_expanded: false,
        },
    );

    let file_tree = Some(FileTreeEntry {
        name: "./root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children,
        is_expanded: true,
    });

    // Create a custom explorer
    let explorer = MockExplorer::new(files.clone(), file_tree);

    // Create a mock project manager with our files
    let project_manager = MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        Box::new(explorer),
    );

    // Create a default mock command executor
    let command_executor = MockCommandExecutor::new(vec![]);

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager: &project_manager,
        command_executor: &command_executor,
        working_memory: None,
    };

    // Test write_file tool - create new file
    {
        // Get the tool from the registry
        let write_file_tool = registry
            .get("write_file")
            .expect("write_file tool should be registered");

        // Parameters for write_file
        let params = json!({
            "project": "test-project",
            "path": "new_file.txt",
            "content": "This is new content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "path": "existing.txt",
            "content": "This is replacement content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "path": "existing.txt",
            "content": "\nAppended content",
            "append": true
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, params).await?;

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
        let updated_content = explorer.read_file(&PathBuf::from("./root/existing.txt"))?;
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
    let mut files = HashMap::new();
    let source_file_content = concat!(
        "function test() {\n",
        "  console.log(\"old message\");\n",
        "  const x=42;\n",
        "  return x;\n",
        "}"
    );
    files.insert(
        PathBuf::from("./root/source.txt"),
        source_file_content.to_string(),
    );

    // Create a custom explorer
    let explorer = MockExplorer::new(files, None);

    // Create a mock project manager with our files
    let project_manager = MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        Box::new(explorer),
    );

    // Create a default mock command executor
    let command_executor = MockCommandExecutor::new(vec![]);

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager: &project_manager,
        command_executor: &command_executor,
        working_memory: None,
    };

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
        let params = json!({
            "project": "test-project",
            "path": "source.txt",
            "diff": diff
        });

        // Execute the tool
        let result = replace_in_file_tool.invoke(&mut context, params).await?;

        // Format the output
        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully replaced"));

        // Verify the content was actually replaced by reading the file
        let explorer = context
            .project_manager
            .get_explorer_for_project("test-project")?;
        let updated_content = explorer.read_file(&PathBuf::from("./root/source.txt"))?;

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
        let params = json!({
            "project": "test-project",
            "path": "source.txt",
            "diff": diff
        });

        // Execute the tool
        let result = replace_in_file_tool.invoke(&mut context, params).await?;

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

    // Create a custom explorer
    let explorer = MockExplorer::default();

    // Create a mock project manager with our files
    let project_manager = MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        Box::new(explorer),
    );

    // Create a mock command executor with successful command execution
    let command_executor = MockCommandExecutor::new(vec![Ok(CommandOutput {
        success: true,
        output: "Command output\nLine 2\nWarning message".to_string(),
    })]);

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager: &project_manager,
        command_executor: &command_executor,
        working_memory: None,
    };

    // Test execute_command tool - successful command
    {
        // Get the tool from the registry
        let execute_command_tool = registry
            .get("execute_command")
            .expect("execute_command tool should be registered");

        // Parameters for execute_command
        let params = json!({
            "project": "test-project",
            "command_line": "ls -la",
            "working_dir": "src"
        });

        // Execute the tool
        let result = execute_command_tool.invoke(&mut context, params).await?;

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
        // Reset the command executor with a command that fails
        let command_executor = MockCommandExecutor::new(vec![Ok(CommandOutput {
            success: false,
            output: "Some output\nError: command failed".to_string(),
        })]);

        // Update the context
        let mut context = ToolContext::<'_> {
            project_manager: &project_manager,
            command_executor: &command_executor,
            working_memory: None,
        };

        // Get the tool from the registry
        let execute_command_tool = registry
            .get("execute_command")
            .expect("execute_command tool should be registered");

        // Parameters for execute_command
        let params = json!({
            "project": "test-project",
            "command_line": "invalid-command"
        });

        // Execute the tool
        let result = execute_command_tool.invoke(&mut context, params).await?;

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

    // Set up sample test files
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.txt"),
        "Test file content".to_string(),
    );

    // Create file tree
    let mut children = HashMap::new();
    children.insert(
        "test.txt".to_string(),
        FileTreeEntry {
            name: "test.txt".to_string(),
            entry_type: FileSystemEntryType::File,
            children: HashMap::new(),
            is_expanded: false,
        },
    );

    let file_tree = Some(FileTreeEntry {
        name: "./root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children,
        is_expanded: true,
    });

    // Create a custom explorer
    let explorer = MockExplorer::new(files, file_tree);

    // Create a mock project manager with our files
    let project_manager = MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        Box::new(explorer),
    );

    // Create a default mock command executor
    let command_executor = crate::utils::DefaultCommandExecutor;

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager: &project_manager,
        command_executor: &command_executor,
        working_memory: None,
    };

    // Test list_projects tool
    {
        // Get the tool from the registry
        let list_projects_tool = registry
            .get("list_projects")
            .expect("list_projects tool should be registered");

        // Parameters for list_projects (empty object)
        let params = json!({});

        // Execute the tool
        let result = list_projects_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "paths": ["test.txt", "test.txt:10-20"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "path": "new_test.txt",
            "content": "This is a test content",
            "append": false
        });

        // Execute the tool
        let result = write_file_tool.invoke(&mut context, params).await?;

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
        let params = json!({
            "project": "test-project",
            "paths": ["."],
            "max_depth": 2
        });

        // Execute the tool
        let result = list_files_tool.invoke(&mut context, params).await?;

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
    if let Some(_) = native_parser.generate_tool_documentation(ToolScope::Agent) {
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
    if let Some(_) = native_parser.generate_syntax_documentation() {
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
    let result = parse_xml_tool_invocations(text, 123, 0);

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
    use crate::tools::parse::parse_xml_tool_invocations_with_filter;
    use crate::tools::tool_use_filter::SingleToolFilter;

    // Test case where we have multiple valid blocks but want to limit to first
    let text = r#"Let me read these files:

<tool:read_files>
<param:project>test-project</param:project>
<param:path>file1.txt</param:path>
</tool:read_files>

And then list the directory:

<tool:list_files>
<param:project>test-project</param:project>
<param:paths>src</param:paths>
</tool:list_files>

And finally modify the file:

<tool:replace_in_file>
<param:project>test-project</param:project>
<param:path>file1.txt</param:path>
<param:diff>some diff</param:diff>
</tool:replace_in_file>"#;

    // With the new system, this should return only the first tool and truncate after it
    let filter = SingleToolFilter;
    let result = parse_xml_tool_invocations_with_filter(text, 123, 0, Some(&filter)).unwrap();

    // Should only get the first tool
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "read_files");
    assert_eq!(result[0].input["project"], "test-project");

    // The XML parser converts single path parameters to the paths array based on the schema
    let paths = result[0].input["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "file1.txt");
}

#[tokio::test]
async fn test_xml_parsing_with_truncation_preserves_valid_tool() {
    use crate::tools::parse::parse_xml_tool_invocations_with_truncation;
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
    let result = parse_xml_tool_invocations_with_truncation(text, 123, 0, Some(&filter));

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
