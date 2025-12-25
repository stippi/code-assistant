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
pub use render::{Render, ResourcesTracker};
pub use result::ToolResult;
pub use spec::{ToolScope, ToolSpec};
pub use title::generate_tool_title;
pub use tool::{Tool, ToolContext};
