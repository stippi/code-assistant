pub mod acp;

pub use acp::AcpPermissionMediator;

// The permission types live with the generic tool core; re-exported here so
// the historical `crate::permissions::*` paths keep working.
pub use tools_core::permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
};
