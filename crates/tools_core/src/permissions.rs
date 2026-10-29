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
    /// Let a [`ToolCallReviewer`] judge every call that may change state
    /// against what the user asked for; ask the user only when it blocks a
    /// call (or cannot judge it). Read-only tools and file edits run
    /// without review unless they are [`capabilities::OUTWARD`].
    Auto,
}

impl PermissionTier {
    /// Whether invoking a tool with the given spec requires asking first.
    pub fn requires_permission(&self, spec: &ToolSpec) -> bool {
        match self {
            PermissionTier::BypassAll => false,
            PermissionTier::OutwardTools => spec.has_capability(capabilities::OUTWARD),
            PermissionTier::WriteTools => !spec.has_capability(capabilities::READ_ONLY),
            PermissionTier::AllTools => true,
            PermissionTier::Auto => {
                spec.has_capability(capabilities::OUTWARD)
                    || !(spec.has_capability(capabilities::READ_ONLY)
                        || spec.has_capability(capabilities::EDITS_FILES))
            }
        }
    }
}

/// How [`ToolPermissions::check`] cleared a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clearance {
    /// No question needed, or the user (or a session grant) allowed it.
    Allowed,
    /// The [`ToolCallReviewer`] allowed it, having seen its parameters.
    /// The tool's own escalations those parameters ask for are covered too
    /// (see [`ReviewedCallMediator`]).
    Reviewed,
}

/// The mediator a tool gets for a call the reviewer allowed. The reviewer
/// saw the parameters, among them a request to run a command outside the
/// sandbox (`ExecuteCommand`), so that escalation is granted without asking
/// again; every other question still goes to the user.
pub struct ReviewedCallMediator {
    inner: Arc<dyn PermissionMediator>,
}

impl ReviewedCallMediator {
    pub fn new(inner: Arc<dyn PermissionMediator>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl PermissionMediator for ReviewedCallMediator {
    async fn request_permission(
        &self,
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision> {
        match request.reason {
            PermissionRequestReason::ExecuteCommand { .. } => Ok(PermissionDecision::GrantedOnce),
            _ => self.inner.request_permission(request).await,
        }
    }
}

/// A tool call as presented to a [`ToolCallReviewer`].
pub struct ToolCallReview<'a> {
    pub spec: &'a ToolSpec,
    pub params: &'a serde_json::Value,
    /// What the user said in this conversation, oldest first. Tool results
    /// are left out, so text the agent merely read cannot authorize a call.
    pub user_messages: &'a [String],
}

/// A reviewer's judgment of a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    Allow,
    /// Not clearly what the user asked for, or too risky to run unasked.
    Block {
        reason: String,
    },
}

/// Judges tool calls on the user's behalf under [`PermissionTier::Auto`],
/// typically by asking a (fast) model.
#[async_trait]
pub trait ToolCallReviewer: Send + Sync {
    async fn review(&self, call: ToolCallReview<'_>) -> Result<ReviewVerdict>;
}

/// Per-session permission state handed to the agent loop: the active tier
/// plus the tools the user granted for the rest of the session.
///
/// Cloning shares the grant set (it is session-scoped state), so the loop's
/// parallel execution path and sub-agents all see the same grants.
#[derive(Clone, Default)]
pub struct ToolPermissions {
    pub tier: PermissionTier,
    /// Judges calls under [`PermissionTier::Auto`]. Without one, auto mode
    /// asks the user about every call it would have reviewed.
    pub reviewer: Option<Arc<dyn ToolCallReviewer>>,
    granted_tools: Arc<Mutex<HashSet<String>>>,
}

impl ToolPermissions {
    pub fn new(tier: PermissionTier) -> Self {
        Self {
            tier,
            reviewer: None,
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
    /// `user_messages` yields what the user said in the conversation; it is
    /// only called when a [`ToolCallReviewer`] judges the call.
    ///
    /// Returns how the invocation was cleared to proceed. The `Err` message is
    /// routed back to the LLM as the tool result, so it tells the model how
    /// to react to a denial.
    pub async fn check(
        &self,
        handler: Option<&dyn PermissionMediator>,
        spec: &ToolSpec,
        tool_id: Option<&str>,
        params: &serde_json::Value,
        user_messages: impl FnOnce() -> Vec<String>,
    ) -> Result<Clearance> {
        if !self.tier.requires_permission(spec) {
            return Ok(Clearance::Allowed);
        }
        if self.is_granted(&spec.name) {
            return Ok(Clearance::Allowed);
        }
        // Under auto mode, the reviewer decides; the user is only asked
        // about what it blocks, together with its reason.
        let blocked = if self.tier == PermissionTier::Auto {
            let reason = match &self.reviewer {
                Some(reviewer) => {
                    let user_messages = user_messages();
                    let review = ToolCallReview {
                        spec,
                        params,
                        user_messages: &user_messages,
                    };
                    match reviewer.review(review).await {
                        Ok(ReviewVerdict::Allow) => return Ok(Clearance::Reviewed),
                        Ok(ReviewVerdict::Block { reason }) => reason,
                        Err(error) => format!("The auto-mode review failed: {error:#}"),
                    }
                }
                None => "Auto mode has no reviewer.".to_string(),
            };
            Some(reason)
        } else {
            None
        };
        let Some(handler) = handler else {
            match blocked {
                Some(reason) => anyhow::bail!(
                    "Auto mode blocked tool '{}': {reason} \
                     Do not retry the same call; try a different approach or ask the user.",
                    spec.name
                ),
                None => anyhow::bail!(
                    "Permission is required to run tool '{}', but this frontend cannot ask the user. \
                     The call was denied by policy.",
                    spec.name
                ),
            }
        };
        let reason = match &blocked {
            Some(reason) => PermissionRequestReason::AutoModeBlocked { params, reason },
            None => PermissionRequestReason::ToolInvocation { params },
        };
        let decision = handler
            .request_permission(PermissionRequest {
                tool_id,
                tool_name: &spec.name,
                reason,
            })
            .await?;
        match decision {
            PermissionDecision::GrantedOnce => Ok(Clearance::Allowed),
            PermissionDecision::GrantedSession | PermissionDecision::GrantedPersistent => {
                self.grant(&spec.name);
                Ok(Clearance::Allowed)
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
    /// Under [`PermissionTier::Auto`], the reviewer blocked the call (or
    /// could not judge it); the user decides instead.
    AutoModeBlocked {
        params: &'a serde_json::Value,
        reason: &'a str,
    },
    /// Approve loading a project's local `.mcp.json`, which launches the MCP
    /// servers it defines — i.e. runs project-supplied commands. Not tied to a
    /// tool invocation; raised before a project's registry is built.
    TrustLocalMcp {
        project_dir: &'a Path,
        server_names: Vec<String>,
    },
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
    /// Allow this single invocation; ask again next time.
    GrantedOnce,
    /// Allow for the rest of this session (in-memory grant).
    GrantedSession,
    /// Allow and remember durably — the embedder persists the approval so it
    /// survives across sessions and restarts (e.g. trusting a project's local
    /// `.mcp.json`). Ordinary tier-based tool prompts do not offer this; a
    /// tool that receives it degrades to a session grant, since there is no
    /// durable per-tool grant store.
    GrantedPersistent,
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
                Vec::new,
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
                Vec::new,
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
            .check(Some(&mediator), &spec_with(&[]), None, &params, Vec::new)
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
            .check(Some(&mediator), &spec, None, &params, Vec::new)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params, Vec::new)
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
            .check(Some(&mediator), &spec, None, &params, Vec::new)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params, Vec::new)
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 2);
    }

    #[tokio::test]
    async fn missing_handler_denies_when_tier_requires_asking() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params, Vec::new)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot ask the user"));
    }

    /// Reviewer returning a fixed verdict and recording what it was shown.
    struct ScriptedReviewer {
        verdict: Result<ReviewVerdict, String>,
        seen: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedReviewer {
        fn new(verdict: Result<ReviewVerdict, String>) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                seen: Mutex::default(),
            })
        }
    }

    #[async_trait]
    impl ToolCallReviewer for ScriptedReviewer {
        async fn review(&self, call: ToolCallReview<'_>) -> Result<ReviewVerdict> {
            self.seen.lock().unwrap().push(call.user_messages.to_vec());
            self.verdict.clone().map_err(|e| anyhow::anyhow!(e))
        }
    }

    /// Mediator recording the reason of each request.
    #[derive(Default)]
    struct RecordingMediator {
        reasons: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl PermissionMediator for RecordingMediator {
        async fn request_permission(
            &self,
            request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            self.reasons
                .lock()
                .unwrap()
                .push(format!("{:?}", request.reason));
            Ok(PermissionDecision::GrantedOnce)
        }
    }

    fn auto_permissions(reviewer: Option<Arc<ScriptedReviewer>>) -> ToolPermissions {
        let mut permissions = ToolPermissions::new(PermissionTier::Auto);
        permissions.reviewer = reviewer.map(|r| r as Arc<dyn ToolCallReviewer>);
        permissions
    }

    #[test]
    fn auto_reviews_everything_but_local_reads_and_edits() {
        let tier = PermissionTier::Auto;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(!tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(tier.requires_permission(&spec_with(&[])));
        assert!(tier.requires_permission(&spec_with(&[
            capabilities::READ_ONLY,
            capabilities::OUTWARD
        ])));
        assert_eq!(
            serde_json::from_str::<PermissionTier>("\"auto\"").unwrap(),
            PermissionTier::Auto
        );
    }

    #[tokio::test]
    async fn auto_runs_what_the_reviewer_allows_without_asking() {
        let reviewer = ScriptedReviewer::new(Ok(ReviewVerdict::Allow));
        let permissions = auto_permissions(Some(reviewer.clone()));
        let mediator = RecordingMediator::default();
        let params = serde_json::json!({});
        permissions
            .check(Some(&mediator), &spec_with(&[]), None, &params, || {
                vec!["run the tests".to_string()]
            })
            .await
            .unwrap();
        assert!(mediator.reasons.lock().unwrap().is_empty());
        assert_eq!(
            *reviewer.seen.lock().unwrap(),
            vec![vec!["run the tests".to_string()]]
        );
    }

    #[tokio::test]
    async fn auto_reports_reviewed_calls_and_their_mediator_grants_escalations() {
        let reviewer = ScriptedReviewer::new(Ok(ReviewVerdict::Allow));
        let permissions = auto_permissions(Some(reviewer));
        let params = serde_json::json!({});
        let clearance = permissions
            .check(None, &spec_with(&[]), None, &params, Vec::new)
            .await
            .unwrap();
        assert_eq!(clearance, Clearance::Reviewed);

        let inner = Arc::new(ScriptedMediator::new(PermissionDecision::Denied));
        let mediator = ReviewedCallMediator::new(inner.clone());
        let escalation = mediator
            .request_permission(PermissionRequest {
                tool_id: None,
                tool_name: "execute_command",
                reason: PermissionRequestReason::ExecuteCommand {
                    command_line: "find ~/Downloads -type f",
                    working_dir: None,
                },
            })
            .await
            .unwrap();
        assert_eq!(escalation, PermissionDecision::GrantedOnce);
        let other = mediator
            .request_permission(PermissionRequest {
                tool_id: None,
                tool_name: "browser",
                reason: PermissionRequestReason::ToolInvocation { params: &params },
            })
            .await
            .unwrap();
        assert_eq!(other, PermissionDecision::Denied);
        assert_eq!(inner.call_count(), 1);
    }

    #[tokio::test]
    async fn auto_does_not_review_read_only_tools() {
        let reviewer = ScriptedReviewer::new(Ok(ReviewVerdict::Allow));
        let permissions = auto_permissions(Some(reviewer.clone()));
        let params = serde_json::json!({});
        permissions
            .check(
                None,
                &spec_with(&[capabilities::READ_ONLY]),
                None,
                &params,
                || panic!("no review, no transcript"),
            )
            .await
            .unwrap();
        assert!(reviewer.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn auto_asks_the_user_about_blocked_calls_with_the_reason() {
        let reviewer = ScriptedReviewer::new(Ok(ReviewVerdict::Block {
            reason: "pushes to main".into(),
        }));
        let permissions = auto_permissions(Some(reviewer));
        let mediator = RecordingMediator::default();
        let params = serde_json::json!({});
        permissions
            .check(Some(&mediator), &spec_with(&[]), None, &params, Vec::new)
            .await
            .unwrap();
        let reasons = mediator.reasons.lock().unwrap();
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("AutoModeBlocked"));
        assert!(reasons[0].contains("pushes to main"));
    }

    #[tokio::test]
    async fn auto_escalates_when_the_review_fails_or_no_reviewer_is_set() {
        let params = serde_json::json!({});
        for permissions in [
            auto_permissions(Some(ScriptedReviewer::new(Err("timeout".into())))),
            auto_permissions(None),
        ] {
            let mediator = RecordingMediator::default();
            permissions
                .check(Some(&mediator), &spec_with(&[]), None, &params, Vec::new)
                .await
                .unwrap();
            assert!(mediator.reasons.lock().unwrap()[0].contains("AutoModeBlocked"));
        }
    }

    #[tokio::test]
    async fn auto_without_a_mediator_returns_the_block_reason_to_the_model() {
        let reviewer = ScriptedReviewer::new(Ok(ReviewVerdict::Block {
            reason: "deletes the home directory".into(),
        }));
        let permissions = auto_permissions(Some(reviewer));
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params, Vec::new)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("Auto mode blocked"));
        assert!(message.contains("deletes the home directory"));
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
