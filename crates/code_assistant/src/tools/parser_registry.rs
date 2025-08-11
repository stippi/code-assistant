//! Parser registry for different tool invocation syntaxes

use crate::agent::ToolSyntax;
use crate::tools::{
    parse_caret_tool_invocations, parse_xml_tool_invocations, tool_use_filter::SmartToolFilter,
    ToolRequest,
};
use crate::ui::streaming::StreamProcessorTrait;
use crate::ui::UserInterface;
use anyhow::Result;
use llm::{ContentBlock, LLMResponse};
use std::sync::Arc;

/// Trait for parsing tool invocations from LLM responses
pub trait ToolInvocationParser: Send + Sync {
    /// Extract `ToolRequest`s from a complete LLM response and return truncated response.
    /// Implementations may inspect either the raw text blocks, the `ToolUse`
    /// blocks, or both.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// A stream-processor that renders *this syntax* for the UI.
    fn stream_processor(
        &self,
        ui: Arc<dyn UserInterface>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait>;

    /// Generate tool documentation in this parser's syntax format.
    /// Returns None if this parser doesn't need tool documentation (e.g., Native mode).
    fn generate_tool_documentation(&self, scope: crate::tools::core::ToolScope) -> Option<String>;

    /// Generate syntax documentation explaining how to use this parser's format.
    /// Returns None if this parser doesn't need syntax documentation (e.g., Native mode).
    fn generate_syntax_documentation(&self) -> Option<String>;
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
        if let ContentBlock::Text { text } = block {
            // Parse Caret tool invocations and get truncation position
            let (block_tool_requests, truncated_text) =
                parse_caret_tool_invocations(text, request_id, tool_requests.len(), Some(&filter))?;

            tool_requests.extend(block_tool_requests.clone());

            // If tools were found in this text block, use truncated text
            if !block_tool_requests.is_empty() {
                truncated_content.push(ContentBlock::Text {
                    text: truncated_text,
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
        if let ContentBlock::Text { text } = block {
            // Parse XML tool invocations and get truncation position
            let (block_tool_requests, truncated_text) =
                parse_xml_tool_invocations(text, request_id, tool_requests.len(), Some(&filter))?;

            tool_requests.extend(block_tool_requests.clone());

            // If tools were found in this text block, use truncated text
            if !block_tool_requests.is_empty() {
                truncated_content.push(ContentBlock::Text {
                    text: truncated_text,
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

/// Extract JSON tool requests from LLM response and return both requests and original response
fn parse_json_response(
    response: &LLMResponse,
    _request_id: u64,
) -> Result<(Vec<ToolRequest>, LLMResponse)> {
    let mut tool_requests = Vec::new();

    for block in &response.content {
        if let ContentBlock::ToolUse { id, name, input } = block {
            let tool_request = ToolRequest {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            };
            tool_requests.push(tool_request);
        }
    }

    Ok((tool_requests, response.clone()))
}

/// XML-based tool invocation parser
pub struct XmlParser;

impl ToolInvocationParser for XmlParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_xml_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn UserInterface>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::StreamProcessorTrait;
        use crate::ui::streaming::XmlStreamProcessor;
        Box::new(XmlStreamProcessor::new(ui, request_id))
    }

    fn generate_tool_documentation(&self, scope: crate::tools::core::ToolScope) -> Option<String> {
        // Generate XML-style documentation
        Some(self.generate_xml_tool_documentation(scope))
    }

    fn generate_syntax_documentation(&self) -> Option<String> {
        Some(self.generate_xml_syntax_documentation())
    }
}

impl XmlParser {
    fn generate_xml_tool_documentation(&self, scope: crate::tools::core::ToolScope) -> String {
        use crate::tools::core::ToolRegistry;

        let registry = ToolRegistry::global();
        let tool_defs = registry.get_tool_definitions_for_scope(scope);

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
                self.generate_xml_parameter_example(&mut example, name, prop);
            }

            // Then add optional parameters
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| !required_fields.contains(&name.as_str()))
            {
                self.generate_xml_parameter_example(&mut example, name, prop);
            }
        }

        example.push_str(&format!("</tool:{tool_name}>\n"));
        example
    }

    fn generate_xml_parameter_example(
        &self,
        example: &mut String,
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

        // Check if this is a multiline content parameter
        let is_multiline =
            name == "content" || name == "command_line" || name == "diff" || name == "message";

        // Generate appropriate placeholder text
        let placeholder = if is_multiline {
            format!("\nYour {name} here\n")
        } else if name == "project" {
            "project-name".to_string()
        } else if name == "path" || name == "paths" {
            "File path here".to_string()
        } else if name == "regex" {
            "Your regex pattern here".to_string()
        } else if name == "command_line" {
            "Your command here".to_string()
        } else if name == "working_dir" {
            "Working directory here (optional)".to_string()
        } else if name == "url" {
            "https://example.com/docs".to_string()
        } else if name == "query" {
            "Your search query here".to_string()
        } else if name == "hits_page_number" {
            "1".to_string()
        } else if name == "max_depth" {
            "level (optional)".to_string()
        } else {
            format!("{name} here")
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

impl ToolInvocationParser for CaretParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_and_truncate_caret_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn UserInterface>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::CaretStreamProcessor;
        use crate::ui::streaming::StreamProcessorTrait;
        Box::new(CaretStreamProcessor::new(ui, request_id))
    }

    fn generate_tool_documentation(&self, scope: crate::tools::core::ToolScope) -> Option<String> {
        // Generate caret-style documentation
        Some(self.generate_caret_tool_documentation(scope))
    }

    fn generate_syntax_documentation(&self) -> Option<String> {
        Some(self.generate_caret_syntax_documentation())
    }
}

impl CaretParser {
    fn generate_caret_tool_documentation(&self, scope: crate::tools::core::ToolScope) -> String {
        use crate::tools::core::ToolRegistry;

        let registry = ToolRegistry::global();
        let tool_defs = registry.get_tool_definitions_for_scope(scope);

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
                self.generate_caret_parameter_example(&mut example, name, prop);
            }

            // Then add optional parameters
            for (name, prop) in properties
                .iter()
                .filter(|(name, _)| !required_fields.contains(&name.as_str()))
            {
                self.generate_caret_parameter_example(&mut example, name, prop);
            }
        }

        example.push_str("^^^\n");
        example
    }

    fn generate_caret_parameter_example(
        &self,
        example: &mut String,
        name: &str,
        prop: &serde_json::Value,
    ) {
        // Determine if this parameter is an array
        let is_array = prop.get("type").and_then(|t| t.as_str()) == Some("array");

        // Check if this is a multiline content parameter
        let is_multiline = name == "content"
            || name == "command_line"
            || name == "diff"
            || name == "message"
            || name == "old_text"
            || name == "new_text";

        // Generate appropriate placeholder text
        let placeholder = if name == "project" {
            "project-name".to_string()
        } else if name == "path" || name == "paths" {
            "File path here".to_string()
        } else if name == "regex" {
            "Your regex pattern here".to_string()
        } else if name == "command_line" {
            "Your command here".to_string()
        } else if name == "working_dir" {
            "Working directory here (optional)".to_string()
        } else if name == "url" {
            "https://example.com/docs".to_string()
        } else if name == "query" {
            "Your search query here".to_string()
        } else if name == "hits_page_number" {
            "1".to_string()
        } else if name == "max_depth" {
            "level (optional)".to_string()
        } else {
            format!("{name} here")
        };

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

/// JSON-based (native) tool invocation parser
pub struct JsonParser;

impl ToolInvocationParser for JsonParser {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        req_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        parse_json_response(response, req_id)
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn UserInterface>,
        request_id: u64,
    ) -> Box<dyn StreamProcessorTrait> {
        use crate::ui::streaming::JsonStreamProcessor;
        use crate::ui::streaming::StreamProcessorTrait;
        Box::new(JsonStreamProcessor::new(ui, request_id))
    }

    fn generate_tool_documentation(&self, _scope: crate::tools::core::ToolScope) -> Option<String> {
        // Native mode uses API-provided tool definitions, no custom documentation needed
        None
    }

    fn generate_syntax_documentation(&self) -> Option<String> {
        // Native mode uses API-provided function calls, no custom syntax documentation needed
        None
    }
}

/// Registry for tool invocation parsers
pub struct ParserRegistry;

impl ParserRegistry {
    /// Get the appropriate parser for the given tool syntax
    pub fn get(syntax: ToolSyntax) -> Box<dyn ToolInvocationParser> {
        match syntax {
            ToolSyntax::Xml => Box::new(XmlParser),
            ToolSyntax::Native => Box::new(JsonParser),
            ToolSyntax::Caret => Box::new(CaretParser),
        }
    }
}
