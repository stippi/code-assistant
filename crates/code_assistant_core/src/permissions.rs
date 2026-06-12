// The permission types live with the generic tool core; re-exported here so
// the historical `permissions::*` paths keep working. The ACP permission
// mediator lives with the ACP frontend code in the binary.
pub use tools_core::permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
};
