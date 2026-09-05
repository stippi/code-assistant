//! The domain-facing tool API surface.
//!
//! The generic tool infrastructure (Tool trait, registry, rendering, specs,
//! title templating) lives in the `tools_core` crate. This module re-exports
//! it alongside the domain-side pieces (`ToolScope`, the `scope:*` capability
//! tags, and the tools configuration) so tools and call sites have a single
//! place to import from.

pub use tools_core::{
    ImageData, MAX_IMAGE_EDGE, Render, ResourcesTracker, Tool, ToolContext, ToolError,
    ToolRegistry, ToolResult, ToolSpec, cap_base64_image, cap_image_data, generate_tool_title,
};

// Domain-side pieces that historically lived here.
pub use crate::tools::config::ToolsConfig;
pub use crate::tools::scope::{ToolScope, capabilities};
