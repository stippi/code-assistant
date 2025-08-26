//! Tool formatter system for regenerating tool blocks in different syntaxes

use crate::agent::ToolSyntax;
use crate::tools::ToolRequest;
use anyhow::Result;
use serde_json::Value;

/// Trait for formatting tool requests into their string representation in different syntaxes
pub trait ToolFormatter {
    /// Format a tool request as a string in the appropriate syntax
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String>;
}

/// Formatter for Native tool syntax (JSON-based)
pub struct NativeFormatter;

impl ToolFormatter for NativeFormatter {
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String> {
        // Native tools are represented as JSON function calls
        // This is used internally by the LLM API, so we create a representation
        // that matches what would be in the message history
        let mut params = String::new();
        if let Value::Object(map) = &request.input {
            for (key, value) in map {
                let param_value = match value {
                    Value::String(s) => s.clone(),
                    _ => serde_json::to_string(value)?,
                };
                params.push_str(&format!("<parameter name=\"{}\">{}</parameter>\n", key, param_value));
            }
        }
        
        let formatted = format!(
            "<function_calls>\n<invoke name=\"{}\">\n{}</invoke>\n</function_calls>",
            request.name, params
        );
        Ok(formatted)
    }
}

/// Formatter for XML tool syntax
pub struct XmlFormatter;

impl ToolFormatter for XmlFormatter {
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String> {
        let mut formatted = format!("<tool:{}>\n", request.name);
        
        if let Value::Object(map) = &request.input {
            for (key, value) in map {
                let param_value = match value {
                    Value::String(s) => s.clone(),
                    _ => serde_json::to_string(value)?,
                };
                formatted.push_str(&format!("<param:{}>{}</param:{}>\n", key, param_value, key));
            }
        }
        
        formatted.push_str(&format!("</tool:{}>\n", request.name));
        Ok(formatted)
    }
}

/// Formatter for Caret tool syntax
pub struct CaretFormatter;

impl ToolFormatter for CaretFormatter {
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String> {
        let mut formatted = format!("^^^{}\n", request.name);
        
        if let Value::Object(map) = &request.input {
            for (key, value) in map {
                let param_value = match value {
                    Value::String(s) => s.clone(),
                    _ => serde_json::to_string(value)?,
                };
                formatted.push_str(&format!("{}: {}\n", key, param_value));
            }
        }
        
        formatted.push_str("^^^\n");
        Ok(formatted)
    }
}

/// Get the appropriate formatter for a tool syntax
pub fn get_formatter(syntax: ToolSyntax) -> Box<dyn ToolFormatter> {
    match syntax {
        ToolSyntax::Native => Box::new(NativeFormatter),
        ToolSyntax::Xml => Box::new(XmlFormatter),
        ToolSyntax::Caret => Box::new(CaretFormatter),
    }
}

/// Format multiple tool requests in the given syntax
pub fn format_tool_requests(requests: &[ToolRequest], syntax: ToolSyntax) -> Result<String> {
    let formatter = get_formatter(syntax);
    let mut result = String::new();
    
    for request in requests {
        result.push_str(&formatter.format_tool_request(request)?);
        result.push('\n');
    }
    
    Ok(result)
}
