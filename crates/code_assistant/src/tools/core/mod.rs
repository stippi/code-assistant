// Core tools implementation
pub mod spec;
pub mod render;
pub mod tool;
pub mod dyn_tool;
pub mod registry;

// Re-export all core components for easier imports
pub use spec::{ToolMode, ToolSpec};
pub use render::{Render, ResourcesTracker};
pub use tool::{Tool, ToolContext};
pub use dyn_tool::{AnyOutput, DynTool};
pub use registry::ToolRegistry;
