//! A reusable agent core: the agent loop with pluggable behavior.
//!
//! Applications embed [`runtime::AgentRuntime`] and bring their own tools
//! (via a `tools_core::ToolRegistry`), their own behavior plugins (the hook
//! traits in [`hooks`]), their own UI adapter ([`ui::AgentUi`]), their own
//! persistence ([`persistence::SnapshotPersistence`]), and — optionally —
//! their own tool invocation format ([`dialect::ToolDialect`]; the built-in
//! default is native tool calling, [`native::NativeDialect`]).
//!
//! Application state rides on the loop type-erased (`extensions` slots, a
//! dyn-Any approach) — no generics infect the embedding application.

pub mod dialect;
pub mod hooks;
pub mod native;
pub mod persistence;
pub mod runtime;
pub mod tree;
pub mod types;
pub mod ui;

pub use dialect::ToolDialect;
pub use persistence::{AgentSnapshot, SnapshotPersistence};
pub use runtime::{AgentRuntime, AgentRuntimeComponents};
pub use tree::{ConversationPath, MessageNode, NodeId};
pub use types::{
    ParseError, PromptTooLongError, SerializedToolExecution, ToolExecution, ToolRequest,
    text_summary_from_blocks, to_tool_definition, to_tool_definitions,
};
pub use ui::{
    AgentActivity, AgentUi, AgentUiEvent, DisplayFragment, HiddenTools, StreamProcessorTrait,
    ToolStatus, UIError,
};
