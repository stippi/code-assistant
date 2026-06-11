//! Compatibility layer over the extracted `tools_core` crate.
//!
//! The generic tool infrastructure (Tool trait, registry, rendering, specs,
//! title templating) lives in the `tools_core` crate now. This module keeps
//! the historical `crate::tools::core::*` import paths working and adds the
//! domain-side pieces (`ToolScope`, the `scope:*` capability tags, and the
//! tools configuration).

pub use tools_core::{render, tool};

pub use tools_core::{
    generate_tool_title, AnnotatedToolDefinition, AnyOutput, ImageData, Render, ResourcesTracker,
    Tool, ToolContext, ToolError, ToolRegistry, ToolResult, ToolSpec,
};

// Domain-side pieces that historically lived here.
pub use crate::tools::config::ToolsConfig;
pub use crate::tools::scope::{capabilities, ToolScope};
