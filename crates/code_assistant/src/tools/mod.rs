// Original tools implementation
mod parse;
mod types;

// Parser registry for different tool syntaxes
pub mod parser_registry;

// System message generation
pub mod system_message;

// Tool use filtering system
pub mod tool_use_filter;

// New trait-based tools implementation
pub mod core;
pub mod impls;

#[cfg(test)]
mod tests;

pub use parse::{
    parse_caret_tool_invocations, parse_caret_tool_invocations_with_filter,
    parse_caret_tool_invocations_with_truncation, parse_xml_tool_invocations,
    parse_xml_tool_invocations_with_filter, parse_xml_tool_invocations_with_truncation,
};
pub use parser_registry::ParserRegistry;
pub use system_message::generate_system_message;
pub use types::{AnnotatedToolDefinition, ParseError, ToolRequest};
