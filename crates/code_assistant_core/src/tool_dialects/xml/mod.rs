//! The XML tool dialect: `<tool:name>` blocks with `<param:...>` tags.

mod formatter;
mod parser;
pub mod stream;

#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod tests;

pub use parser::parse_xml_tool_invocations;
pub use stream::XmlStreamProcessor;

use crate::tool_dialects::{example_placeholder, is_multiline_param, message_text_segments};
use crate::tools::core::ToolRegistry;
use crate::tools::tool_use_filter::SmartToolFilter;
use crate::tools::ToolRequest;
use agent_core::dialect::ToolDialect;
use agent_core::ui::{AgentUi, HiddenTools, StreamProcessorTrait};
use anyhow::Result;
use llm::{ContentBlock, LLMResponse, Message};
use std::sync::Arc;
use tracing::debug;

/// Parse XML tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_xml_response(
    response: &LLMResponse,
    request_id: u64,
    registry: &ToolRegistry,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();
    let filter = SmartToolFilter::new(registry);

    for block in &response.content {
        if let ContentBlock::Text { text, .. } = block {
            // Parse XML tool invocations and get truncation position
            let (block_tool_requests, truncated_text) = parse_xml_tool_invocations(
                text,
                request_id,
                tool_requests.len(),
                Some(&filter),
                registry,
            )?;

            tool_requests.extend(block_tool_requests.clone());

            // If tools were found in this text block, use truncated text
            if !block_tool_requests.is_empty() {
                truncated_content.push(ContentBlock::Text {
                    text: truncated_text,
                    start_time: None,
                    end_time: None,
                });
                break; // Stop processing after first tool block
            } else {
                // No tools in this block, keep original text
                truncated_content.push(block.clone());
            }
        } else {
            // Keep other blocks (Thinking, etc.) if no tools found yet
            if tool_requests.is_empty() {
                truncated_content.push(block.clone());
            }
        }
    }

    // Create truncated response
    let truncated_response = LLMResponse {
        content: truncated_content,
        usage: response.usage.clone(),
        rate_limit_info: response.rate_limit_info.clone(),
    };

    Ok((tool_requests, truncated_response))
}

/// XML-based tool invocation parser
pub struct XmlDialect;

impl ToolDialect for XmlDialect {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
        registry: &ToolRegistry,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_xml_response(response, req_id, registry)
    }

    fn format_tool_request(
        &self,
        request: &ToolRequest,
        registry: &ToolRegistry,
    ) -> Result<String> {
        formatter::format_tool_request(request, registry)
    }

    fn uses_native_tools(&self) -> bool {
        false
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: HiddenTools,
        registry: Arc<ToolRegistry>,
    ) -> Box<dyn StreamProcessorTrait> {
        Box::new(XmlStreamProcessor::new(
            ui,
            request_id,
            hidden_tools,
            registry,
        ))
    }

    fn render_tool_section_for_prompt(
        &self,
        registry: &ToolRegistry,
        capability: &str,
    ) -> Option<String> {
        // Generate XML-style documentation
        Some(self.generate_xml_tool_documentation(registry, capability))
    }

    fn render_format_section_for_prompt(&self) -> Option<String> {
        Some(self.generate_xml_syntax_documentation())
    }

    fn message_contains_invocation(&self, message: &Message, registry: &ToolRegistry) -> bool {
        let request_id = message.request_id.unwrap_or(0);
        for text in message_text_segments(message) {
            if !text.contains("<tool:") {
                continue;
            }
            match parse_xml_tool_invocations(text, request_id, 0, None, registry) {
                Ok((requests, _)) => {
                    if !requests.is_empty() {
                        return true;
                    }
                }
                Err(error) => {
                    debug!("Failed to parse XML tool invocation while inspecting message: {error}");
                    return true;
                }
            }
        }
        false
    }
}

impl XmlDialect {
    fn generate_xml_tool_documentation(&self, registry: &ToolRegistry, capability: &str) -> String {
        let tool_defs = registry.get_tool_definitions_with_capability(capability);

        let mut docs = String::new();

        for tool in tool_defs {
            // Skip tools with no parameters
            if !tool
                .parameters
                .get("properties")
                .is_some_and(|p| p.is_object())
            {
                continue;
            }

            // Tool header
            docs.push_str(&format!("## {}\n", tool.name));
            docs.push_str(&format!("Description: {}\n", tool.description));

            // Tool parameters
            docs.push_str("Parameters:\n");
            docs.push_str(&self.generate_xml_parameters_doc(&tool.parameters));
            docs.push('\n');

            // Tool usage
            docs.push_str("Usage:\n");
            docs.push_str(&self.generate_xml_usage_example(&tool.name, &tool.parameters, registry));
            docs.push('\n');
        }

        docs
    }

    fn generate_xml_parameters_doc(&self, parameters: &serde_json::Value) -> String {
        let mut docs = Vec::new();

        // Get the properties object
        if let Some(properties) = parameters.get("properties").and_then(|p| p.as_object()) {
            // Get required fields array
            let required_fields = parameters
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                .unwrap_or_default();

            // Process each parameter
            for (name, param) in properties {
                let is_required = required_fields.contains(&name.as_str());
                docs.push(self.format_xml_parameter_doc(name, param, is_required));
            }
        }

        docs.join("\n")
    }

    fn format_xml_parameter_doc(
        &self,
        name: &str,
        param: &serde_json::Value,
        is_required: bool,
    ) -> String {
        let mut doc = format!("- {name}");

        // Add required flag if needed
        if is_required {
            doc.push_str(" (required)");
        }

        // Add description if available
        if let Some(description) = param.get("description") {
            if let Some(desc_str) = description.as_str() {
                // Remove any (required) markers from the description since we handle it separately
                let desc_str = desc_str.replace("(required)", "").trim().to_string();
                doc.push_str(&format!(": {desc_str}"));
            }
        }

        doc
    }

    fn generate_xml_usage_example(
        &self,
        tool_name: &str,
        parameters: &serde_json::Value,
        registry: &ToolRegistry,
    ) -> String {
        let mut example = format!("<tool:{tool_name}>\n");

        // Get the properties object
        if let Some(properties) = parameters.get("properties").and_then(|p| p.as_object()) {
            // Get required fields array
            let required_fields = parameters
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                .unwrap_or_default();

            // Add required parameters first
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| required_fields.contains(&name.as_str()))
            {
                self.generate_xml_parameter_example(&mut example, tool_name, name, prop, registry);
            }

            // Then add optional parameters
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| !required_fields.contains(&name.as_str()))
            {
                self.generate_xml_parameter_example(&mut example, tool_name, name, prop, registry);
            }
        }

        example.push_str(&format!("</tool:{tool_name}>\n"));
        example
    }

    fn generate_xml_parameter_example(
        &self,
        example: &mut String,
        tool_name: &str,
        name: &str,
        prop: &serde_json::Value,
        registry: &ToolRegistry,
    ) {
        // Determine if this parameter is an array
        let is_array = prop.get("type").and_then(|t| t.as_str()) == Some("array");

        // For array parameters, we always use the singular form in the XML tags
        let param_name = if is_array && name.ends_with('s') {
            // Simple singular conversion by removing trailing 's'
            &name[..name.len() - 1]
        } else {
            name
        };

        // Generate appropriate placeholder text
        let placeholder = if is_multiline_param(tool_name, name, registry) {
            format!("\nYour {name} here\n")
        } else {
            example_placeholder(name, prop)
        };

        // Add the parameter to the example
        example.push_str(&format!(
            "<param:{param_name}>{placeholder}</param:{param_name}>\n"
        ));

        // For array types, add a second example parameter to show multiple items
        if is_array {
            example.push_str(&format!(
                "<param:{param_name}>Another {name} here</param:{param_name}>\n"
            ));
        }
    }

    fn generate_xml_syntax_documentation(&self) -> String {
        r#"# Tool Use Formatting

Tool use is formatted using XML-style tags. The tool name is prefixed by 'tool:' and enclosed in opening and closing tags, and each parameter is similarly prefixed with 'param:' and enclosed within its own set of tags. For array parameters, simply repeat the same parameter for each item. Here's the structure:

<tool:tool_name>
<param:parameter1_name>value1</param:parameter1_name>
<param:parameter2_name>
value can stretch
multiple lines
</param:parameter2_name>
<param:array_param>item1</param:array_param>
<param:array_param>item2</param:array_param>
...
</tool:tool_name>

For example:

<tool:read_files>
<param:project>frontend</param:project>
<param:path>src/main.js</param:path>
<param:path>src/utils.js</param:path>
</tool:read_files>

Always adhere to this format for the tool use to ensure proper parsing and execution."#.to_string()
    }
}
