//! Permission mediation for potentially sensitive tool operations.

use crate::spec::{ToolSpec, capabilities};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

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
    /// Ask before running tools whose effects leave the machine — anything
    /// tagged [`capabilities::OUTWARD`]. Local state changes run without
    /// prompting; the tag wins over [`capabilities::READ_ONLY`].
    OutwardTools,
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
            PermissionTier::OutwardTools => spec.has_capability(capabilities::OUTWARD),
            PermissionTier::WriteTools => !spec.has_capability(capabilities::READ_ONLY),
            PermissionTier::AllTools => true,
        }
    }
}

/// Per-session permission state handed to the agent loop: the active tier
/// plus the tools the user granted for the rest of the session.
///
/// Cloning shares the grant set (it is session-scoped state), so the loop's
/// parallel execution path and sub-agents all see the same grants.
#[derive(Clone, Default)]
pub struct ToolPermissions {
    pub tier: PermissionTier,
    granted_tools: Arc<Mutex<HashSet<String>>>,
}

impl ToolPermissions {
    pub fn new(tier: PermissionTier) -> Self {
        Self {
            tier,
            granted_tools: Arc::default(),
        }
    }

    pub fn is_granted(&self, tool_name: &str) -> bool {
        self.granted_tools.lock().unwrap().contains(tool_name)
    }

    pub fn grant(&self, tool_name: &str) {
        self.granted_tools
            .lock()
            .unwrap()
            .insert(tool_name.to_string());
    }

    /// Tier-based gate run by the agent loop before invoking a tool.
    ///
    /// Returns `Ok(())` when the invocation may proceed. The `Err` message is
    /// routed back to the LLM as the tool result, so it tells the model how
    /// to react to a denial.
    pub async fn check(
        &self,
        handler: Option<&dyn PermissionMediator>,
        spec: &ToolSpec,
        tool_id: Option<&str>,
        params: &serde_json::Value,
    ) -> Result<()> {
        if !self.tier.requires_permission(spec) {
            return Ok(());
        }
        if self.is_granted(&spec.name) {
            return Ok(());
        }
        let Some(handler) = handler else {
            anyhow::bail!(
                "Permission is required to run tool '{}', but this frontend cannot ask the user. \
                 The call was denied by policy.",
                spec.name
            );
        };
        let decision = handler
            .request_permission(PermissionRequest {
                tool_id,
                tool_name: &spec.name,
                reason: PermissionRequestReason::ToolInvocation { params },
            })
            .await?;
        match decision {
            PermissionDecision::GrantedOnce => Ok(()),
            PermissionDecision::GrantedSession => {
                self.grant(&spec.name);
                Ok(())
            }
            PermissionDecision::Denied => anyhow::bail!(
                "The user denied permission to run tool '{}'. Do not simply retry; \
                 ask the user how to proceed or try a different approach.",
                spec.name
            ),
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mediator returning a fixed decision and counting how often it is asked.
    struct ScriptedMediator {
        decision: PermissionDecision,
        calls: AtomicUsize,
    }

    impl ScriptedMediator {
        fn new(decision: PermissionDecision) -> Self {
            Self {
                decision,
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl PermissionMediator for ScriptedMediator {
        async fn request_permission(
            &self,
            _request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.decision)
        }
    }

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
    fn outward_tools_requires_permission_only_for_outward_tagged_tools() {
        let tier = PermissionTier::OutwardTools;
        assert!(tier.requires_permission(&spec_with(&[capabilities::OUTWARD])));
        // Outward wins over read-only: reading via an outward service still
        // leaks the request to a third party.
        assert!(tier.requires_permission(&spec_with(&[
            capabilities::READ_ONLY,
            capabilities::OUTWARD
        ])));
        assert!(!tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(!tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn outward_tools_tier_serializes_as_kebab_case() {
        assert_eq!(
            serde_json::to_string(&PermissionTier::OutwardTools).unwrap(),
            "\"outward-tools\""
        );
        let parsed: PermissionTier = serde_json::from_str("\"outward-tools\"").unwrap();
        assert_eq!(parsed, PermissionTier::OutwardTools);
    }

    #[test]
    fn all_tools_always_requires_permission() {
        let tier = PermissionTier::AllTools;
        assert!(tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[tokio::test]
    async fn bypass_tier_does_not_consult_the_mediator() {
        let permissions = ToolPermissions::new(PermissionTier::BypassAll);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[capabilities::EDITS_FILES]),
                None,
                &params,
            )
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 0);
    }

    #[tokio::test]
    async fn write_tier_skips_read_only_tools() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[capabilities::READ_ONLY]),
                None,
                &params,
            )
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 0);
    }

    #[tokio::test]
    async fn denied_decision_becomes_an_error() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        let err = permissions
            .check(Some(&mediator), &spec_with(&[]), None, &params)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied permission"));
        assert_eq!(mediator.call_count(), 1);
    }

    #[tokio::test]
    async fn granted_session_is_remembered_per_tool() {
        let permissions = ToolPermissions::new(PermissionTier::AllTools);
        let mediator = ScriptedMediator::new(PermissionDecision::GrantedSession);
        let params = serde_json::json!({});
        let spec = spec_with(&[capabilities::READ_ONLY]);
        permissions
            .check(Some(&mediator), &spec, None, &params)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params)
            .await
            .unwrap();
        // Second call is served from the session grant, and clones share it.
        assert_eq!(mediator.call_count(), 1);
        assert!(permissions.clone().is_granted("test_tool"));
    }

    #[tokio::test]
    async fn granted_once_asks_again_next_time() {
        let permissions = ToolPermissions::new(PermissionTier::AllTools);
        let mediator = ScriptedMediator::new(PermissionDecision::GrantedOnce);
        let params = serde_json::json!({});
        let spec = spec_with(&[]);
        permissions
            .check(Some(&mediator), &spec, None, &params)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params)
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 2);
    }

    #[tokio::test]
    async fn missing_handler_denies_when_tier_requires_asking() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot ask the user"));
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
