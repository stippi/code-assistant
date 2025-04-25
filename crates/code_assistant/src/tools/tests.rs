use crate::tests::mocks::{MockExplorer, MockProjectManager};
use crate::tools::core::{ToolContext, ToolRegistry};
use crate::tools::impls::{ListFilesTool, ListProjectsTool, ReadFilesTool, WriteFileTool};
use crate::types::{FileSystemEntryType, FileTreeEntry};

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

// Make the MockProjectManager public so it can be used by other tests

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
