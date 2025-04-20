use crate::tools::core::{ToolContext, ToolRegistry};
use crate::tools::impls::{ListFilesTool, ListProjectsTool, ReadFilesTool, WriteFileTool};
use crate::tools::parse::{parse_tool_json, parse_tool_xml};
use crate::tools::tests::mocks::{MockExplorer, MockProjectManager};
use crate::types::{FileSystemEntryType, FileTreeEntry, Tool as LegacyTool};

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
    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        explorer,
    ));

    // Create a default mock command executor
    let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

    // Create a tool context
    let mut context = ToolContext::<'_> {
        project_manager,
        command_executor,
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

#[tokio::test]
async fn test_parse_to_legacy_tool_to_new_tool() -> Result<()> {
    // 1. Test parsing XML to legacy Tool
    let xml = r#"<tool:read_files>
    <param:project>test-project</param:project>
    <param:path>file1.txt</param:path>
    <param:path>file2.txt:10-20</param:path>
</tool:read_files>"#;

    let parsed_tool = parse_tool_xml(xml)?;
    match &parsed_tool {
        LegacyTool::ReadFiles { project, paths } => {
            assert_eq!(project, "test-project");
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0], PathBuf::from("file1.txt"));
            assert_eq!(paths[1], PathBuf::from("file2.txt:10-20"));
        }
        _ => panic!("Expected ReadFiles tool"),
    }

    // 2. Test parsing JSON to legacy Tool
    let json_value = json!({
        "tool": "read_files",
        "params": {
            "project": "test-project",
            "paths": ["file1.txt", "file2.txt:10-20"]
        }
    });

    let parsed_tool = parse_tool_json("read_files", &json_value["params"])?;
    match &parsed_tool {
        LegacyTool::ReadFiles { project, paths } => {
            assert_eq!(project, "test-project");
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0], PathBuf::from("file1.txt"));
            assert_eq!(paths[1], PathBuf::from("file2.txt:10-20"));
        }
        _ => panic!("Expected ReadFiles tool"),
    }

    // 3. Test that we can simulate parsing input for the new Tool system
    // Note: This is a demonstration of how we could connect the legacy parsing
    // to the new Tool system in an adapter

    // Get the ReadFilesTool from the registry
    let tool_registry = ToolRegistry::global();

    if let Some(read_files_tool) = tool_registry.get("read_files") {
        // Extract parameters from the parsed legacy tool
        if let LegacyTool::ReadFiles { project, paths } = parsed_tool {
            // Convert to the format expected by the new system
            let path_strings: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            let params = json!({
                "project": project,
                "paths": path_strings
            });

            // Set up a proper mock explorer with test files
            let mut files = HashMap::new();
            files.insert(
                PathBuf::from("./root/file1.txt"),
                "File 1 content".to_string(),
            );
            files.insert(
                PathBuf::from("./root/file2.txt"),
                "Line 1\nLine 2\nLine 3\nLine 4\nLine 5".to_string(),
            );

            let explorer = MockExplorer::new(files, None);

            // Create a mock context with our test files
            let project_manager = Box::new(MockProjectManager::default().with_project(
                "test-project",
                PathBuf::from("./root"),
                explorer,
            ));

            // Create a default command executor
            let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

            let mut context = ToolContext::<'_> {
                project_manager,
                command_executor,
                working_memory: None,
            };

            // Execute the tool - this would be done in the adapter
            let result = read_files_tool.invoke(&mut context, params).await?;

            // Check that we can render the result
            let mut tracker = crate::tools::core::ResourcesTracker::new();
            let output = result.as_render().render(&mut tracker);

            assert!(output.contains("Successfully loaded"));
        }
    } else {
        panic!("Expected read_files tool to be registered");
    }

    Ok(())
}
