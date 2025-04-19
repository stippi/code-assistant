use crate::tools::core::{ToolContext, ToolRegistry};
use crate::tools::tests::mocks::{MockExplorer, MockProjectManager};
use crate::types::WorkingMemory;
use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::test]
async fn test_read_files_updates_memory() -> Result<()> {
    // Create a tool registry
    let registry = ToolRegistry::global();

    // Get the read_files tool
    let read_files_tool = registry
        .get("read_files")
        .expect("read_files tool should be registered");

    // Create test files
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.txt"),
        "Test file content".to_string(),
    );
    files.insert(
        PathBuf::from("./root/test2.txt"),
        "Another file content".to_string(),
    );

    // Create a mock explorer with these files
    let explorer = MockExplorer::new(files, None);

    // Create a mock project manager with our test files
    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        PathBuf::from("./root"),
        explorer,
    ));

    // Create working memory
    let mut working_memory = WorkingMemory::default();

    // Create a tool context with working memory
    let mut context = ToolContext::<'_> {
        project_manager,
        working_memory: Some(&mut working_memory),
    };

    // Parameters for read_files
    let params = json!({
        "project": "test-project",
        "paths": ["test.txt", "test2.txt"]
    });

    // Execute the tool
    let result = read_files_tool.invoke(&mut context, params).await?;

    // Format the output
    let mut tracker = crate::tools::core::ResourcesTracker::new();
    let output = result.as_render().render(&mut tracker);

    // Check the output
    assert!(output.contains("Successfully loaded"));

    // Verify that the files were added to working memory
    assert_eq!(working_memory.loaded_resources.len(), 2);

    // Check that both files are in the working memory
    assert!(working_memory
        .loaded_resources
        .contains_key(&("test-project".to_string(), "test.txt".into())));
    assert!(working_memory
        .loaded_resources
        .contains_key(&("test-project".to_string(), "test2.txt".into())));

    Ok(())
}
