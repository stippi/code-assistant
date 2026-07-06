//! Permission mediation for potentially sensitive tool operations.

use crate::spec::{ToolSpec, capabilities};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// When to ask the user for permission before running a tool.
///
/// The tier only decides *whether* to ask; the actual question is routed
/// through the session's [`PermissionMediator`]. Tools may additionally
/// request permission for specific escalations (e.g. `execute_command`
/// asking to bypass the sandbox) independently of the tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionTier {
    /// Never ask; every tool call runs without prompting.
    #[default]
    BypassAll,
    /// Ask before running tools that may modify state — anything not
    /// tagged [`capabilities::READ_ONLY`].
    WriteTools,
    /// Ask before running any tool.
    AllTools,
}

impl PermissionTier {
    /// Whether invoking a tool with the given spec requires asking first.
    pub fn requires_permission(&self, spec: &ToolSpec) -> bool {
        match self {
            PermissionTier::BypassAll => false,
            PermissionTier::WriteTools => !spec.has_capability(capabilities::READ_ONLY),
            PermissionTier::AllTools => true,
        }
    }
}

/// Context about why permission is being requested.
#[derive(Debug)]
pub enum PermissionRequestReason<'a> {
    ExecuteCommand {
        command_line: &'a str,
        working_dir: Option<&'a Path>,
    },
    /// Tier-based gate before invoking a tool ([`PermissionTier`]).
    ToolInvocation { params: &'a serde_json::Value },
}

/// Request payload passed to a [`PermissionMediator`].
#[derive(Debug)]
pub struct PermissionRequest<'a> {
    pub tool_id: Option<&'a str>,
    pub tool_name: &'a str,
    pub reason: PermissionRequestReason<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    GrantedOnce,
    GrantedSession,
    Denied,
}

#[async_trait]
pub trait PermissionMediator: Send + Sync {
    async fn request_permission(
        &self,
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(tags: &'static [&'static str]) -> ToolSpec {
        ToolSpec {
            name: "test_tool".into(),
            description: "test".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
            annotations: None,
            capabilities: ToolSpec::capabilities(tags),
            multiline_params: &[],
            hidden: false,
            title_template: None,
        }
    }

    #[test]
    fn bypass_all_never_requires_permission() {
        let tier = PermissionTier::BypassAll;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(!tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(!tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn write_tools_requires_permission_unless_read_only() {
        let tier = PermissionTier::WriteTools;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        // Untagged tools (e.g. MCP tools without a read-only hint) count as writes.
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn all_tools_always_requires_permission() {
        let tier = PermissionTier::AllTools;
        assert!(tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn tier_serializes_as_kebab_case_and_defaults_to_bypass_all() {
        assert_eq!(PermissionTier::default(), PermissionTier::BypassAll);
        assert_eq!(
            serde_json::to_string(&PermissionTier::WriteTools).unwrap(),
            "\"write-tools\""
        );
        let parsed: PermissionTier = serde_json::from_str("\"all-tools\"").unwrap();
        assert_eq!(parsed, PermissionTier::AllTools);
    }
}
