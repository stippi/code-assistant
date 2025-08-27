//! Tool formatter system for regenerating tool blocks in different syntaxes

use crate::agent::ToolSyntax;
use crate::tools::parser_registry::is_multiline_param;
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
        // Return the input serialized as JSON string
        Ok(serde_json::to_string(&request.input)?)
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
                if is_multiline_param(key) {
                    formatted.push_str(&format!("<param:{key}>\n{param_value}\n</param:{key}>\n"));
                } else {
                    formatted.push_str(&format!("<param:{key}>{param_value}</param:{key}>\n"));
                }
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
                if is_multiline_param(key) {
                    formatted.push_str(&format!("{key} ---\n{param_value}\n--- {key}\n"));
                } else {
                    formatted.push_str(&format!("{key}: {param_value}\n"));
                }
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
