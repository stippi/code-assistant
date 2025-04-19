use crate::tools::core::{DynTool, Tool, ToolContext, ToolRegistry};
use crate::tools::impls::{ListProjectsTool, ReadFilesTool};
use crate::tools::parse::{parse_tool_json, parse_tool_xml};
use crate::types::{CodeExplorer, Project, Tool as LegacyTool};

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// Helper MockProjectManager for testing
struct MockProjectManager {
    projects: HashMap<String, Project>,
}

impl MockProjectManager {
    fn new() -> Self {
        let mut projects = HashMap::new();
        projects.insert(
            "test-project".to_string(),
            Project {
                path: PathBuf::from("/mock/root"),
            },
        );

        Self { projects }
    }
}

#[async_trait::async_trait]
impl crate::config::ProjectManager for MockProjectManager {
    fn add_temporary_project(&mut self, _path: PathBuf) -> Result<String> {
        Ok("temp-project".to_string())
    }

    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        Ok(self.projects.clone())
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        Ok(self.projects.get(name).cloned())
    }

    fn get_explorer_for_project(&self, _name: &str) -> Result<Box<dyn CodeExplorer>> {
        // Return a minimal mock explorer that's sufficient for tests
        struct MockExplorer {
            root_path: PathBuf,
        }

        impl CodeExplorer for MockExplorer {
            fn root_dir(&self) -> PathBuf {
                self.root_path.clone()
            }

            fn read_file(&self, _path: &PathBuf) -> Result<String> {
                Ok("Mock file content".to_string())
            }

            fn read_file_range(
                &self,
                _path: &PathBuf,
                _start_line: Option<usize>,
                _end_line: Option<usize>,
            ) -> Result<String> {
                Ok("Mock file content (range)".to_string())
            }

            fn write_file(&self, _path: &PathBuf, _content: &String, _append: bool) -> Result<()> {
                Ok(())
            }

            fn delete_file(&self, _path: &PathBuf) -> Result<()> {
                Ok(())
            }

            fn create_initial_tree(
                &mut self,
                _max_depth: usize,
            ) -> Result<crate::types::FileTreeEntry> {
                unimplemented!()
            }

            fn list_files(
                &mut self,
                _path: &PathBuf,
                _max_depth: Option<usize>,
            ) -> Result<crate::types::FileTreeEntry> {
                unimplemented!()
            }

            fn apply_replacements(
                &self,
                _path: &Path,
                _replacements: &[crate::types::FileReplacement],
            ) -> Result<String> {
                unimplemented!()
            }

            fn search(
                &self,
                _path: &Path,
                _options: crate::types::SearchOptions,
            ) -> Result<Vec<crate::types::SearchResult>> {
                unimplemented!()
            }
        }

        Ok(Box::new(MockExplorer {
            root_path: PathBuf::from("/mock/root"),
        }))
    }
}

#[tokio::test]
async fn test_tool_dispatch_via_registry() -> Result<()> {
    // Create a tool registry
    let mut registry = ToolRegistry::new();

    // Register tools manually rather than using the global registry
    registry.register(Box::new(ListProjectsTool));
    registry.register(Box::new(ReadFilesTool));

    // Create a mock project manager
    let project_manager = Box::new(MockProjectManager::new());

    // Create a tool context
    let mut context = ToolContext { project_manager };

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

            // Create a mock context
            let project_manager = Box::new(MockProjectManager::new());
            let mut context = ToolContext { project_manager };

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
