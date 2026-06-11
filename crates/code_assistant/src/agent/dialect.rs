//! Compatibility re-export: the tool dialect abstraction lives in
//! `agent_core` (§3.7 of the extraction plan). The XML and Caret
//! implementations remain application code (`crate::tools::parser_registry`);
//! the native default ships with the core.

pub use agent_core::dialect::ToolDialect;
