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

// Make the MockProjectManager public so it can be used by other tests

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
        explorer,
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
        explorer,
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
        explorer,
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
        explorer,
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
        explorer,
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
