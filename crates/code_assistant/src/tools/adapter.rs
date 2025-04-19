use crate::config::ProjectManager;
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry};
use crate::types::{Tool, ToolResult};
use anyhow::{anyhow, Result};

/// Converts a tool result from the new system to the legacy ToolResult enum
fn convert_to_legacy_result(
    tool: &Tool,
    result: Box<dyn crate::tools::core::AnyOutput>,
) -> Result<ToolResult> {
    let mut tracker = ResourcesTracker::new();
    let output = result.as_render().render(&mut tracker);

    match tool {
        Tool::ListProjects => {
            use crate::config;
            // For ListProjects, we need to parse the output or directly access the projects
            let projects = config::load_projects()?;
            Ok(ToolResult::ListProjects { projects })
        }
        // Other conversion cases will be added as we implement more tools
        _ => Err(anyhow!("Unsupported tool type for adapter: {:?}", tool)),
    }
}

/// Execute a legacy Tool using the new system
pub async fn execute_with_new_system(
    tool: &Tool,
    project_manager: Box<dyn ProjectManager>,
) -> Result<ToolResult> {
    // Create tool context
    let mut context = ToolContext {
        project_manager,
    };

    // Get the tool registry
    let registry = ToolRegistry::global();

    match tool {
        Tool::ListProjects => {
            if let Some(list_projects_tool) = registry.get("list_projects") {
                // Empty parameters for list_projects
                let params = serde_json::json!({});

                // Execute the tool
                let result = list_projects_tool.invoke(&mut context, params).await?;

                // Convert the result back to the old format
                convert_to_legacy_result(tool, result)
            } else {
                Err(anyhow!("list_projects tool not found in registry"))
            }
        },
        // Other tool mappings will be added as we implement more tools
        _ => Err(anyhow!("Tool not yet implemented in new system: {:?}", tool)),
    }
}
