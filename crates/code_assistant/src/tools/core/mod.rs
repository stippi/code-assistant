// Core tools implementation
pub mod config;
pub mod dyn_tool;
pub mod registry;
pub mod render;
pub mod result;
pub mod spec;
pub mod title;
pub mod tool;

// Re-export all core components for easier imports
pub use config::ToolsConfig;
pub use dyn_tool::AnyOutput;
pub use registry::ToolRegistry;
pub use render::{ImageData, Render, ResourcesTracker};
pub use result::{ToolError, ToolResult};
pub use spec::{AnnotatedToolDefinition, ToolSpec};
pub use title::generate_tool_title;
pub use tool::{Tool, ToolContext};

// Compatibility re-exports: the scope selection vocabulary is domain-side
// (see `crate::tools::scope`), but callers historically import it from here.
pub use crate::tools::scope::{capabilities, ToolScope};
