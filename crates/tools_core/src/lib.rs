//! Generic tool infrastructure: the `Tool` trait, the type-erased `DynTool`,
//! an instantiable `ToolRegistry`, output rendering, capability-tagged tool
//! specs, and title templating.
//!
//! This crate is application-agnostic. Applications define their own tools,
//! fill their own registry instances, and pass application-specific services
//! to tools through `ToolContext::extensions` (a type-erased extension slot).
//! Tool selection is expressed through free-form capability tags on
//! [`ToolSpec`]; the crate prescribes no scoping vocabulary beyond the
//! generic tags in [`spec::capabilities`].

pub mod dyn_tool;
pub mod permissions;
pub mod registry;
pub mod render;
pub mod result;
pub mod spec;
pub mod title;
pub mod tool;

pub use dyn_tool::{AnyOutput, DynTool};
pub use permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
    PermissionTier, ToolPermissions,
};
pub use registry::ToolRegistry;
pub use render::{ImageData, Render, ResourcesTracker};
pub use result::{ToolError, ToolResult};
pub use spec::{AnnotatedToolDefinition, ToolSpec, capabilities};
pub use title::{format_parameter_for_title, generate_title_from_template, generate_tool_title};
pub use tool::{Tool, ToolContext};
