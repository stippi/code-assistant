//! The XML, Caret, and native implementations of [`ToolDialect`], plus the
//! syntax-based selection helper.

use crate::agent::dialect::ToolDialect;
use crate::agent::ToolSyntax;
use crate::tools::core::ToolRegistry;
use crate::tools::formatter::{CaretFormatter, ToolFormatter, XmlFormatter};
use crate::tools::{
    parse_caret_tool_invocations, parse_xml_tool_invocations, tool_use_filter::SmartToolFilter,
    ToolRequest,
};
use crate::ui::streaming::{HiddenTools, StreamProcessorTrait};
use agent_core::AgentUi;
use anyhow::Result;
use llm::{ContentBlock, LLMResponse, Message, MessageContent};
use std::sync::Arc;
use tracing::debug;

fn message_text_segments(message: &Message) -> Vec<&str> {
    match &message.content {
        MessageContent::Text(text) => vec![text.as_str()],
        MessageContent::Structured(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text, .. } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect(),
    }
}

/// Parse Caret tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_caret_response(
    response: &LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();
    let filter = SmartToolFilter::new();

    for block in &response.content {
        if let ContentBlock::Text { text, .. } = block {
            // Parse Caret tool invocations and get truncation position
            let (block_tool_requests, truncated_text) = parse_caret_tool_invocations(
                text,
                request_id,
                tool_requests.len(),
                Some(&filter),
                crate::tools::global_registry(),
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

/// Parse XML tool requests from LLM response and return both requests and truncated response after first tool
fn parse_and_truncate_xml_response(
    response: &LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();
    let mut truncated_content = Vec::new();
    let filter = SmartToolFilter::new();

    for block in &response.content {
        if let ContentBlock::Text { text, .. } = block {
            // Parse XML tool invocations and get truncation position
            let (block_tool_requests, truncated_text) = parse_xml_tool_invocations(
                text,
                request_id,
                tool_requests.len(),
                Some(&filter),
                crate::tools::global_registry(),
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

/// Whether the named parameter of the given tool typically spans multiple
/// lines (block syntax in the text dialects).
fn is_multiline_param(tool_name: &str, param_name: &str) -> bool {
    crate::tools::global_registry()
        .get(tool_name)
        .map(|tool| tool.spec().is_multiline_param(param_name))
        .unwrap_or(false)
}

/// Example value for a parameter in the prompt docs, taken from the JSON
/// schema's `examples` annotation when present.
fn example_placeholder(name: &str, prop: &serde_json::Value) -> String {
    prop.get("examples")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| format!("{name} here"))
}

/// XML-based tool invocation parser
pub struct XmlParser;

impl ToolDialect for XmlParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_xml_response(response, req_id)
    }

    fn format_tool_request(
        &self,
        request: &ToolRequest,
        registry: &ToolRegistry,
    ) -> Result<String> {
        XmlFormatter.format_tool_request(request, registry)
    }

    fn uses_native_tools(&self) -> bool {
        false
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: HiddenTools,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::XmlStreamProcessor;
        Box::new(XmlStreamProcessor::new(ui, request_id, hidden_tools))
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

    fn message_contains_invocation(&self, message: &Message) -> bool {
        let request_id = message.request_id.unwrap_or(0);
        for text in message_text_segments(message) {
            if !text.contains("<tool:") {
                continue;
            }
            match parse_xml_tool_invocations(text, request_id, 0, None, crate::tools::global_registry()) {
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

impl XmlParser {
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
            docs.push_str(&self.generate_xml_usage_example(&tool.name, &tool.parameters));
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
                self.generate_xml_parameter_example(&mut example, tool_name, name, prop);
            }

            // Then add optional parameters
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| !required_fields.contains(&name.as_str()))
            {
                self.generate_xml_parameter_example(&mut example, tool_name, name, prop);
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
        let placeholder = if is_multiline_param(tool_name, name) {
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

/// Caret-based tool invocation parser
pub struct CaretParser;

impl ToolDialect for CaretParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_caret_response(response, req_id)
    }

    fn format_tool_request(
        &self,
        request: &ToolRequest,
        registry: &ToolRegistry,
    ) -> Result<String> {
        CaretFormatter.format_tool_request(request, registry)
    }

    fn uses_native_tools(&self) -> bool {
        false
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: HiddenTools,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::CaretStreamProcessor;
        Box::new(CaretStreamProcessor::new(ui, request_id, hidden_tools))
    }

    fn render_tool_section_for_prompt(
        &self,
        registry: &ToolRegistry,
        capability: &str,
    ) -> Option<String> {
        // Generate caret-style documentation
        Some(self.generate_caret_tool_documentation(registry, capability))
    }

    fn render_format_section_for_prompt(&self) -> Option<String> {
        Some(self.generate_caret_syntax_documentation())
    }

    fn message_contains_invocation(&self, message: &Message) -> bool {
        let request_id = message.request_id.unwrap_or(0);
        for text in message_text_segments(message) {
            if !text.contains("^^^") {
                continue;
            }
            match parse_caret_tool_invocations(text, request_id, 0, None, crate::tools::global_registry()) {
                Ok((requests, _)) => {
                    if !requests.is_empty() {
                        return true;
                    }
                }
                Err(error) => {
                    debug!(
                        "Failed to parse Caret tool invocation while inspecting message: {error}"
                    );
                    return true;
                }
            }
        }
        false
    }
}

impl CaretParser {
    fn generate_caret_tool_documentation(
        &self,
        registry: &ToolRegistry,
        capability: &str,
    ) -> String {
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
            docs.push_str(&self.generate_caret_parameters_doc(&tool.parameters));
            docs.push('\n');

            // Tool usage
            docs.push_str("Usage:\n");
            docs.push_str(&self.generate_caret_usage_example(&tool.name, &tool.parameters));
            docs.push('\n');
        }

        docs
    }

    fn generate_caret_parameters_doc(&self, parameters: &serde_json::Value) -> String {
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
                docs.push(self.format_caret_parameter_doc(name, param, is_required));
            }
        }

        docs.join("\n")
    }

    fn format_caret_parameter_doc(
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

    fn generate_caret_usage_example(
        &self,
        tool_name: &str,
        parameters: &serde_json::Value,
    ) -> String {
        let mut example = format!("^^^{tool_name}\n");

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
                self.generate_caret_parameter_example(&mut example, tool_name, name, prop);
            }

            // Then add optional parameters
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| !required_fields.contains(&name.as_str()))
            {
                self.generate_caret_parameter_example(&mut example, tool_name, name, prop);
            }
        }

        example.push_str("^^^\n");
        example
    }

    fn generate_caret_parameter_example(
        &self,
        example: &mut String,
        tool_name: &str,
        name: &str,
        prop: &serde_json::Value,
    ) {
        // Determine if this parameter is an array
        let is_array = prop.get("type").and_then(|t| t.as_str()) == Some("array");

        let is_multiline = is_multiline_param(tool_name, name);
        let placeholder = example_placeholder(name, prop);

        if is_array {
            // Array parameter
            example.push_str(&format!("{name}: [\n"));
            example.push_str(&format!("{placeholder}\n"));
            example.push_str(&format!("Another {name} here\n"));
            example.push_str("]\n");
        } else if is_multiline {
            // Multiline parameter
            example.push_str(&format!("{name} ---\n"));
            example.push_str(&format!("Your {name} here\n"));
            example.push_str(&format!("--- {name}\n"));
        } else {
            // Simple parameter
            example.push_str(&format!("{name}: {placeholder}\n"));
        }
    }

    fn generate_caret_syntax_documentation(&self) -> String {
        r#"# Tool Use Formatting

Tool use is formatted using triple-caret fenced blocks. The tool name follows the opening fence on the same line. Parameters are specified using `key: value` syntax, with multi-line parameters using `key ---` to start and `--- key` to end. Here's the structure:

^^^tool_name
parameter1: value1
parameter2: value2
multiline_param ---
This can span
multiple lines
--- multiline_param
array_param: [
item1
item2
]
^^^

For example:

^^^read_files
project: frontend
paths: [
src/main.js
src/utils.js
]
^^^

Always adhere to this format for the tool use to ensure proper parsing and execution."#.to_string()
    }
}

/// Registry for tool invocation parsers
pub struct ParserRegistry;

impl ParserRegistry {
    /// Get the appropriate dialect for the given tool syntax
    pub fn get(syntax: ToolSyntax) -> Arc<dyn ToolDialect> {
        match syntax {
            ToolSyntax::Xml => Arc::new(XmlParser),
            ToolSyntax::Native => Arc::new(agent_core::native::NativeDialect),
            ToolSyntax::Caret => Arc::new(CaretParser),
        }
    }
}
