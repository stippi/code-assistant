//! Format-on-save functionality for automatically formatting files after modifications

use crate::agent::types::ToolExecution;
use crate::agent::ToolSyntax;
use crate::config::ProjectManager;
use crate::tools::formatter::get_formatter;
use crate::tools::ToolRequest;
use crate::utils::CommandExecutor;
use anyhow::{anyhow, Result};
use glob::Pattern;
use llm::{ContentBlock, Message, MessageContent, MessageRole};
use serde_json::Value;
use std::path::Path;
use tracing::{debug, warn};

/// Handles format-on-save operations
pub struct FormatOnSaveHandler<'a> {
    project_manager: &'a dyn ProjectManager,
    command_executor: &'a dyn CommandExecutor,
}

impl<'a> FormatOnSaveHandler<'a> {
    pub fn new(
        project_manager: &'a dyn ProjectManager,
        command_executor: &'a dyn CommandExecutor,
    ) -> Self {
        Self {
            project_manager,
            command_executor,
        }
    }

    /// Check if a file should be formatted based on project configuration
    pub fn should_format_file(&self, project_name: &str, file_path: &Path) -> Result<Option<String>> {
        let project = self
            .project_manager
            .get_project(project_name)?
            .ok_or_else(|| anyhow!("Project not found: {}", project_name))?;

        if let Some(format_config) = &project.format_on_save {
            let file_name = file_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            for (pattern, command) in format_config {
                if let Ok(glob_pattern) = Pattern::new(pattern) {
                    if glob_pattern.matches(file_name) {
                        return Ok(Some(command.clone()));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Format a file using the specified command
    pub async fn format_file(&self, project_name: &str, file_path: &Path, command: &str) -> Result<String> {
        let project = self
            .project_manager
            .get_project(project_name)?
            .ok_or_else(|| anyhow!("Project not found: {}", project_name))?;

        debug!("Formatting file {} with command: {}", file_path.display(), command);

        // Execute the format command in the project directory
        let output = self
            .command_executor
            .execute(command, Some(&project.path))
            .await?;

        if !output.success {
            warn!("Format command failed: {}", output.output);
            return Err(anyhow!("Format command failed: {}", output.output));
        }

        // Read the formatted file content
        let explorer = self.project_manager.get_explorer_for_project(project_name)?;
        let formatted_content = explorer.read_file(file_path)?;

        Ok(formatted_content)
    }

    /// Process a tool execution that might need formatting, returning updated parameters if formatting occurred
    pub async fn process_tool_execution(
        &self,
        execution: &ToolExecution,
        tool_syntax: ToolSyntax,
    ) -> Result<Option<ToolRequest>> {
        match execution.tool_request.name.as_str() {
            "write_file" => self.process_write_file_execution(execution, tool_syntax).await,
            "edit" => self.process_edit_execution(execution, tool_syntax).await,
            "replace_in_file" => self.process_replace_in_file_execution(execution, tool_syntax).await,
            _ => Ok(None),
        }
    }

    async fn process_write_file_execution(
        &self,
        execution: &ToolExecution,
        _tool_syntax: ToolSyntax,
    ) -> Result<Option<ToolRequest>> {
        let input = &execution.tool_request.input;
        let project = input["project"].as_str().ok_or_else(|| anyhow!("Missing project parameter"))?;
        let path_str = input["path"].as_str().ok_or_else(|| anyhow!("Missing path parameter"))?;
        let original_content = input["content"].as_str().ok_or_else(|| anyhow!("Missing content parameter"))?;

        let file_path = Path::new(path_str);

        if let Some(command) = self.should_format_file(project, file_path)? {
            let formatted_content = self.format_file(project, file_path, &command).await?;

            // Only update if content actually changed
            if formatted_content != original_content {
                debug!("File content changed after formatting, updating tool parameters");

                let mut new_input = input.clone();
                new_input["content"] = Value::String(formatted_content);

                return Ok(Some(ToolRequest {
                    id: execution.tool_request.id.clone(),
                    name: execution.tool_request.name.clone(),
                    input: new_input,
                }));
            }
        }

        Ok(None)
    }

    async fn process_edit_execution(
        &self,
        execution: &ToolExecution,
        _tool_syntax: ToolSyntax,
    ) -> Result<Option<ToolRequest>> {
        let input = &execution.tool_request.input;
        let project = input["project"].as_str().ok_or_else(|| anyhow!("Missing project parameter"))?;
        let path_str = input["path"].as_str().ok_or_else(|| anyhow!("Missing path parameter"))?;
        let original_new_text = input["new_text"].as_str().ok_or_else(|| anyhow!("Missing new_text parameter"))?;

        let file_path = Path::new(path_str);

        if let Some(command) = self.should_format_file(project, file_path)? {
            let formatted_content = self.format_file(project, file_path, &command).await?;

            // For edit operations, we need to extract the relevant section that was changed
            // This is more complex as we need to figure out what part of the file corresponds to the edit
            // For now, we'll implement a simplified version that assumes the entire new_text was formatted

            // Read the old_text to see what was replaced
            let old_text = input["old_text"].as_str().ok_or_else(|| anyhow!("Missing old_text parameter"))?;

            // Try to find the corresponding formatted section
            if let Some(formatted_new_text) = self.extract_formatted_section(&formatted_content, old_text, original_new_text)? {
                if formatted_new_text != original_new_text {
                    debug!("Edit new_text changed after formatting, updating tool parameters");

                    let mut new_input = input.clone();
                    new_input["new_text"] = Value::String(formatted_new_text);

                    return Ok(Some(ToolRequest {
                        id: execution.tool_request.id.clone(),
                        name: execution.tool_request.name.clone(),
                        input: new_input,
                    }));
                }
            }
        }

        Ok(None)
    }

    async fn process_replace_in_file_execution(
        &self,
        _execution: &ToolExecution,
        _tool_syntax: ToolSyntax,
    ) -> Result<Option<ToolRequest>> {
        // Similar to edit, but handles replace_in_file tool
        // This would need similar logic to extract formatted sections
        // For now, return None (no formatting applied)
        Ok(None)
    }

    /// Extract the formatted section that corresponds to the original new_text
    /// This is a simplified implementation - in practice, this would need more sophisticated logic
    fn extract_formatted_section(
        &self,
        _formatted_content: &str,
        _old_text: &str,
        original_new_text: &str,
    ) -> Result<Option<String>> {
        // For now, just return the original text
        // A real implementation would need to:
        // 1. Find where the old_text was in the original file
        // 2. Determine what the new_text became after formatting
        // 3. Extract that section from the formatted content
        Ok(Some(original_new_text.to_string()))
    }

    /// Update message history to reflect formatted tool calls
    pub fn update_message_history(
        &self,
        messages: &mut Vec<Message>,
        updated_requests: &[(usize, ToolRequest)], // (message_index, updated_request)
        tool_syntax: ToolSyntax,
    ) -> Result<()> {
        for (message_index, updated_request) in updated_requests {
            if let Some(message) = messages.get_mut(*message_index) {
                if message.role == MessageRole::Assistant {
                    // Update the message content to reflect the formatted tool call
                    self.update_message_content(&mut message.content, updated_request, tool_syntax)?;
                }
            }
        }

        Ok(())
    }

    fn update_message_content(
        &self,
        content: &mut MessageContent,
        updated_request: &ToolRequest,
        tool_syntax: ToolSyntax,
    ) -> Result<()> {
        match content {
            MessageContent::Text(text) => {
                // For text content, we need to find and replace the tool call
                // This is complex and would require parsing the existing tool calls
                // For now, we'll implement a simplified version
                *text = self.replace_tool_in_text(text, updated_request, tool_syntax)?;
            }
            MessageContent::Structured(blocks) => {
                // For block content, find the ToolUse block and update it
                for block in blocks {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        if *id == updated_request.id && *name == updated_request.name {
                            *input = updated_request.input.clone();
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn replace_tool_in_text(
        &self,
        text: &str,
        updated_request: &ToolRequest,
        tool_syntax: ToolSyntax,
    ) -> Result<String> {
        // Generate the new formatted tool call
        let formatter = get_formatter(tool_syntax);
        let new_tool_call = formatter.format_tool_request(updated_request)?;

        // For now, return the original text with a comment about formatting
        // A real implementation would need to parse and replace the specific tool call
        Ok(format!("{}\n<!-- Tool call updated after formatting -->\n{}", text, new_tool_call))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::{MockCommandExecutor, MockProjectManager};
    use crate::types::Project;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_should_format_file() {
        let mut project_manager = MockProjectManager::new();
        
        // Set up a project with format-on-save configuration
        let mut format_config = HashMap::new();
        format_config.insert("*.rs".to_string(), "rustfmt".to_string());
        format_config.insert("*.js".to_string(), "prettier --write".to_string());
        
        let project = Project {
            path: PathBuf::from("/tmp/test"),
            format_on_save: Some(format_config),
        };
        
        project_manager.add_project("test-project".to_string(), project);
        
        let command_executor = MockCommandExecutor::new(vec![]);
        let handler = FormatOnSaveHandler::new(&project_manager, &command_executor);
        
        // Test Rust file
        let result = handler.should_format_file("test-project", Path::new("main.rs")).unwrap();
        assert_eq!(result, Some("rustfmt".to_string()));
        
        // Test JavaScript file
        let result = handler.should_format_file("test-project", Path::new("app.js")).unwrap();
        assert_eq!(result, Some("prettier --write".to_string()));
        
        // Test non-matching file
        let result = handler.should_format_file("test-project", Path::new("README.md")).unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test] 
    async fn test_should_format_file_no_config() {
        let mut project_manager = MockProjectManager::new();
        
        // Set up a project without format-on-save configuration
        let project = Project {
            path: PathBuf::from("/tmp/test"),
            format_on_save: None,
        };
        
        project_manager.add_project("test-project".to_string(), project);
        
        let command_executor = MockCommandExecutor::new(vec![]);
        let handler = FormatOnSaveHandler::new(&project_manager, &command_executor);
        
        // Test any file - should return None
        let result = handler.should_format_file("test-project", Path::new("main.rs")).unwrap();
        assert_eq!(result, None);
    }
}
