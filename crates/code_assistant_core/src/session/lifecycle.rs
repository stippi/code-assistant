//! Session lifecycle: the state a sidebar needs to triage sessions rather
//! than merely list them.
//!
//! Two questions are kept apart on purpose. *What is the session doing* is
//! the [`SessionStatus`], derived from the live activity state and open
//! permission requests. *Does it need me* is a separate bit: a session is
//! unread when it changed after the user last looked at it. Colour and
//! emphasis in a frontend follow the status; dimming follows unread.
//!
//! Finished work leaves the inbox by *settling*: by hand, after a period of
//! inactivity, or once the session's branch is merged. Settling hides
//! nothing permanently; a settled session sits in its own shelf and comes
//! back on request. Pulling a session back out blocks automatic settlement
//! until the session sees new activity, so a deliberately kept session does
//! not sink again a day later.
//!
//! The lifecycle is persisted next to the session index
//! (`sessions/lifecycle.json`), never inside the session file: a visit must
//! not rewrite a multi-megabyte conversation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::session::SessionService;
use crate::session::instance::SessionActivityState;
use crate::session::pull_request::{PullRequestSnapshot, PullRequestState};
use crate::utils::file_utils::atomic_write_json;

/// Per-session lifecycle record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionLifecycle {
    /// When a frontend last showed this session to the user.
    pub last_visited_at: Option<SystemTime>,
    /// Set while the session rests in the settled shelf.
    pub settled: Option<Settlement>,
    /// When the user last pulled the session back out of the settled shelf.
    /// Automatic settlement waits for activity newer than this.
    pub unsettled_at: Option<SystemTime>,
    /// The pull request behind the session's branch, as last read from the
    /// GitHub CLI. Refreshed by the lifecycle sweep and after each run.
    pub pull_request: Option<PullRequestSnapshot>,
}

/// How and when a session settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub at: SystemTime,
    pub reason: SettledReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettledReason {
    /// The user settled the session.
    Manual,
    /// No activity for the configured number of days.
    Inactivity,
    /// The session's branch was merged into the repository's base branch.
    BranchMerged,
}

impl SessionLifecycle {
    pub fn is_settled(&self) -> bool {
        self.settled.is_some()
    }

    /// Whether the host reports the session's pull request as merged.
    pub fn pull_request_merged(&self) -> bool {
        self.pull_request
            .as_ref()
            .is_some_and(|pr| pr.state == PullRequestState::Merged)
    }

    /// The user looked at the session.
    pub fn visit(&mut self, now: SystemTime) {
        self.last_visited_at = Some(now);
    }

    /// Move the session into the settled shelf. Settling again keeps the
    /// original time and reason.
    pub fn settle(&mut self, reason: SettledReason, now: SystemTime) {
        if self.settled.is_none() {
            self.settled = Some(Settlement { at: now, reason });
        }
    }

    /// Pull the session back into the inbox. Automatic settlement stays off
    /// until the session sees activity newer than this moment.
    pub fn unsettle(&mut self, now: SystemTime) {
        if self.settled.take().is_some() {
            self.unsettled_at = Some(now);
        }
    }

    /// Whether the session changed after the user last looked at it. A
    /// session that was never visited (created before visits were tracked)
    /// counts as read: a stale mark on every old session would mean nothing.
    pub fn is_unread(&self, updated_at: SystemTime) -> bool {
        match self.last_visited_at {
            Some(visited) => updated_at > visited,
            None => false,
        }
    }

    /// Sort anchor for the inbox: creation time, re-anchored to the moment the
    /// session was last un-settled so it surfaces at the top instead of
    /// sinking back to its creation slot. The inbox is otherwise static —
    /// activity does not move rows, emphasis does.
    pub fn inbox_anchor(&self, created_at: SystemTime) -> SystemTime {
        match self.unsettled_at {
            Some(unsettled) if unsettled > created_at => unsettled,
            _ => created_at,
        }
    }
}

/// Rules for automatic settlement, persisted at `<config_dir>/lifecycle.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    /// Settle a session after this many days without activity; 0 turns the
    /// rule off.
    pub auto_settle_after_days: u32,
    /// Settle a session once its branch is merged into the base branch.
    pub auto_settle_on_merge: bool,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            auto_settle_after_days: 14,
            auto_settle_on_merge: true,
        }
    }
}

impl LifecycleConfig {
    pub fn path() -> PathBuf {
        crate::config_dir::config_dir().join("lifecycle.json")
    }

    /// Load the config; a missing or malformed file yields the defaults.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Failed to parse {}: {e}; using defaults", path.display());
            Self::default()
        })
    }

    pub fn save(&self) -> Result<()> {
        atomic_write_json(&Self::path(), self)
    }

    fn inactivity_threshold(&self) -> Option<Duration> {
        (self.auto_settle_after_days > 0)
            .then(|| Duration::from_secs(u64::from(self.auto_settle_after_days) * 24 * 60 * 60))
    }
}

/// What a session that is not settled looks like from the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// The agent waits for the user to allow a tool call: act now.
    NeedsApproval,
    /// The agent is busy; nothing to do for the user.
    Working,
    /// The agent waits out a rate limit.
    RateLimited,
    /// The agent stopped with an error.
    Failed,
    /// Another code-assistant process runs this session.
    RunningElsewhere,
    /// The agent stopped and waits for the user. The unlabelled resting
    /// state: whether it needs attention is the unread bit, not the status.
    Ready,
}

impl SessionStatus {
    pub fn resolve(activity: &SessionActivityState, awaiting_permission: bool) -> Self {
        if awaiting_permission {
            return Self::NeedsApproval;
        }
        match activity {
            SessionActivityState::Idle => Self::Ready,
            SessionActivityState::AgentRunning | SessionActivityState::WaitingForResponse => {
                Self::Working
            }
            SessionActivityState::RateLimited { .. } => Self::RateLimited,
            SessionActivityState::Errored { .. } => Self::Failed,
            SessionActivityState::RunningExternally => Self::RunningElsewhere,
        }
    }

    /// Whether the session's row should step back visually. Busy sessions
    /// recede because they need nothing; ready sessions recede once read.
    /// Anything that blocks on the user, or broke, stays in front.
    pub fn should_recede(self, unread: bool, is_selected: bool) -> bool {
        if is_selected {
            return false;
        }
        match self {
            Self::Working | Self::RateLimited | Self::RunningElsewhere => true,
            Self::Ready => !unread,
            Self::NeedsApproval | Self::Failed => false,
        }
    }
}

/// The facts the automatic settlement rules look at for one session.
#[derive(Debug, Clone, Copy)]
pub struct SettlementCandidate<'a> {
    pub lifecycle: &'a SessionLifecycle,
    /// The session's last activity.
    pub updated_at: SystemTime,
    /// An agent runs, waits on the user, or holds the session elsewhere.
    pub busy: bool,
    /// The session's branch is merged into the base branch.
    pub branch_merged: bool,
}

/// Decide whether a session settles now, and why. Busy sessions never
/// settle, nor does a session the user pulled back without activity since.
pub fn auto_settlement(
    candidate: SettlementCandidate<'_>,
    now: SystemTime,
    config: &LifecycleConfig,
) -> Option<SettledReason> {
    let lifecycle = candidate.lifecycle;
    if lifecycle.is_settled() || candidate.busy {
        return None;
    }
    if let Some(unsettled) = lifecycle.unsettled_at
        && candidate.updated_at <= unsettled
    {
        return None;
    }
    if candidate.branch_merged && config.auto_settle_on_merge {
        return Some(SettledReason::BranchMerged);
    }
    let threshold = config.inactivity_threshold()?;
    let idle_for = now.duration_since(candidate.updated_at).ok()?;
    (idle_for >= threshold).then_some(SettledReason::Inactivity)
}

/// What the settlement sweep knows about one unsettled session.
#[derive(Debug, Clone)]
pub struct SettlementInput {
    pub session_id: String,
    pub updated_at: SystemTime,
    pub lifecycle: SessionLifecycle,
    /// An agent runs the session, here or in another process.
    pub busy: bool,
    /// The branch the session works on, if it was switched to one.
    pub branch: Option<String>,
    /// The repository to check the branch against, when known.
    pub repo_root: Option<PathBuf>,
}

/// A session working on the base branch itself is never "merged into" it.
/// `base` may be a remote-tracking name such as `origin/main`.
pub fn is_base_branch(branch: &str, base: &str) -> bool {
    branch == base || base.split_once('/').map(|(_, name)| name) == Some(branch)
}

/// The sessions among `inputs` whose branch is merged into their
/// repository's base branch. Repositories are opened once each; a session
/// whose branch cannot be checked is left out.
pub(crate) async fn sessions_with_merged_branch(inputs: &[SettlementInput]) -> HashSet<String> {
    let mut by_repo: HashMap<&PathBuf, Vec<(&str, &str)>> = HashMap::new();
    for input in inputs {
        if let (Some(branch), Some(root)) = (&input.branch, &input.repo_root)
            && !input.busy
        {
            by_repo
                .entry(root)
                .or_default()
                .push((input.session_id.as_str(), branch.as_str()));
        }
    }
    let mut merged = HashSet::new();
    for (root, sessions) in by_repo {
        let repo = match git::GitRepository::open(root) {
            Ok(repo) => repo,
            Err(e) => {
                debug!("Settlement: cannot open {}: {e:#}", root.display());
                continue;
            }
        };
        let Some(base) = repo.default_base_branch() else {
            debug!("Settlement: no base branch in {}", root.display());
            continue;
        };
        for (session_id, branch) in sessions {
            if is_base_branch(branch, &base) {
                continue;
            }
            match repo.is_branch_merged(branch, &base).await {
                Ok(true) => {
                    merged.insert(session_id.to_string());
                }
                Ok(false) => {}
                Err(e) => debug!(
                    "Settlement: cannot check {branch} in {}: {e:#}",
                    root.display()
                ),
            }
        }
    }
    merged
}

/// After a run: remember the branch the session worked on and read its
/// pull request, so the sidebar shows a pull request opened during the run
/// without waiting for the next sweep. The lookup runs without the lock.
pub(crate) async fn refresh_branch_after_run(
    manager: &std::sync::Arc<tokio::sync::Mutex<crate::session::SessionManager>>,
    session_id: &str,
) {
    let observed = manager.lock().await.record_observed_branch(session_id);
    let (root, branch) = match observed {
        Ok(Some(observed)) => observed,
        Ok(None) => return,
        Err(e) => {
            debug!("Cannot record the branch of {session_id}: {e:#}");
            return;
        }
    };
    let fetched = match crate::session::pull_request::fetch_pull_request(&root, &branch).await {
        Ok(fetched) => fetched,
        Err(e) => {
            debug!("Cannot read the pull request of {branch}: {e}");
            return;
        }
    };
    let manager = manager.lock().await;
    if let Err(e) =
        manager.update_session_lifecycle(session_id, |lifecycle| lifecycle.pull_request = fetched)
    {
        debug!("Cannot store the pull request of {session_id}: {e:#}");
    }
}

/// How often the lifecycle sweep runs.
pub const LIFECYCLE_SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Refresh pull requests and apply the automatic settlement rules now and
/// then every [`LIFECYCLE_SWEEP_INTERVAL`], reading the config fresh each
/// time so a settings change applies without a restart. Wiring layers spawn
/// this on the backend runtime.
pub async fn run_lifecycle_sweeper(service: SessionService) {
    loop {
        let config = LifecycleConfig::load();
        match service.sweep_lifecycle(&config).await {
            Ok(settled) if !settled.is_empty() => {
                debug!("Lifecycle sweep settled {} session(s)", settled.len())
            }
            Ok(_) => {}
            Err(e) => warn!("Lifecycle sweep failed: {e:#}"),
        }
        tokio::time::sleep(LIFECYCLE_SWEEP_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[test]
    fn the_base_branch_is_recognised_under_its_remote_name() {
        assert!(is_base_branch("main", "main"));
        assert!(is_base_branch("main", "origin/main"));
        assert!(is_base_branch("feature/x", "origin/feature/x"));
        assert!(!is_base_branch("feature/main", "origin/main"));
        assert!(!is_base_branch("develop", "origin/main"));
    }

    fn t(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_never_visited_session_is_read() {
        let lifecycle = SessionLifecycle::default();
        assert!(!lifecycle.is_unread(t(100)));
    }

    #[test]
    fn a_session_updated_after_the_last_visit_is_unread() {
        let mut lifecycle = SessionLifecycle::default();
        lifecycle.visit(t(50));
        assert!(lifecycle.is_unread(t(60)));
        assert!(!lifecycle.is_unread(t(50)));
        assert!(!lifecycle.is_unread(t(40)));
    }

    #[test]
    fn settling_twice_keeps_the_first_settlement() {
        let mut lifecycle = SessionLifecycle::default();
        lifecycle.settle(SettledReason::Inactivity, t(10));
        lifecycle.settle(SettledReason::Manual, t(20));
        assert_eq!(
            lifecycle.settled,
            Some(Settlement {
                at: t(10),
                reason: SettledReason::Inactivity
            })
        );
    }

    #[test]
    fn unsettling_records_the_moment_and_re_anchors_the_row() {
        let mut lifecycle = SessionLifecycle::default();
        assert_eq!(lifecycle.inbox_anchor(t(5)), t(5));
        lifecycle.unsettle(t(10));
        assert_eq!(lifecycle.unsettled_at, None, "nothing to un-settle");
        lifecycle.settle(SettledReason::Manual, t(10));
        lifecycle.unsettle(t(20));
        assert!(!lifecycle.is_settled());
        assert_eq!(lifecycle.unsettled_at, Some(t(20)));
        assert_eq!(lifecycle.inbox_anchor(t(5)), t(20));
    }

    #[test]
    fn approval_outranks_every_activity_state() {
        for activity in [
            SessionActivityState::Idle,
            SessionActivityState::AgentRunning,
            SessionActivityState::Errored {
                message: "x".into(),
            },
        ] {
            assert_eq!(
                SessionStatus::resolve(&activity, true),
                SessionStatus::NeedsApproval
            );
        }
        assert_eq!(
            SessionStatus::resolve(&SessionActivityState::WaitingForResponse, false),
            SessionStatus::Working
        );
        assert_eq!(
            SessionStatus::resolve(&SessionActivityState::Idle, false),
            SessionStatus::Ready
        );
    }

    #[test]
    fn busy_rows_recede_and_read_ready_rows_recede() {
        assert!(SessionStatus::Working.should_recede(true, false));
        assert!(SessionStatus::Ready.should_recede(false, false));
        assert!(!SessionStatus::Ready.should_recede(true, false));
        assert!(!SessionStatus::NeedsApproval.should_recede(false, false));
        assert!(!SessionStatus::Failed.should_recede(false, false));
        assert!(!SessionStatus::Working.should_recede(true, true));
    }

    fn candidate(lifecycle: &SessionLifecycle, updated_at: SystemTime) -> SettlementCandidate<'_> {
        SettlementCandidate {
            lifecycle,
            updated_at,
            busy: false,
            branch_merged: false,
        }
    }

    #[test]
    fn inactivity_settles_after_the_configured_days() {
        let config = LifecycleConfig {
            auto_settle_after_days: 14,
            auto_settle_on_merge: true,
        };
        let lifecycle = SessionLifecycle::default();
        let updated = t(0);
        assert_eq!(
            auto_settlement(candidate(&lifecycle, updated), updated + 13 * DAY, &config),
            None
        );
        assert_eq!(
            auto_settlement(candidate(&lifecycle, updated), updated + 14 * DAY, &config),
            Some(SettledReason::Inactivity)
        );
    }

    #[test]
    fn zero_days_turns_inactivity_off() {
        let config = LifecycleConfig {
            auto_settle_after_days: 0,
            auto_settle_on_merge: true,
        };
        let lifecycle = SessionLifecycle::default();
        assert_eq!(
            auto_settlement(candidate(&lifecycle, t(0)), t(0) + 400 * DAY, &config),
            None
        );
    }

    #[test]
    fn a_merged_branch_settles_at_once_unless_disabled() {
        let lifecycle = SessionLifecycle::default();
        let merged = SettlementCandidate {
            branch_merged: true,
            ..candidate(&lifecycle, t(0))
        };
        assert_eq!(
            auto_settlement(merged, t(1), &LifecycleConfig::default()),
            Some(SettledReason::BranchMerged)
        );
        let config = LifecycleConfig {
            auto_settle_on_merge: false,
            ..LifecycleConfig::default()
        };
        assert_eq!(auto_settlement(merged, t(1), &config), None);
    }

    #[test]
    fn busy_and_already_settled_sessions_never_settle() {
        let config = LifecycleConfig::default();
        let lifecycle = SessionLifecycle::default();
        let busy = SettlementCandidate {
            busy: true,
            branch_merged: true,
            ..candidate(&lifecycle, t(0))
        };
        assert_eq!(auto_settlement(busy, t(0) + 30 * DAY, &config), None);

        let mut settled = SessionLifecycle::default();
        settled.settle(SettledReason::Manual, t(1));
        assert_eq!(
            auto_settlement(candidate(&settled, t(0)), t(0) + 30 * DAY, &config),
            None
        );
    }

    #[test]
    fn an_unsettled_session_waits_for_new_activity() {
        let config = LifecycleConfig::default();
        let mut lifecycle = SessionLifecycle::default();
        lifecycle.settle(SettledReason::Inactivity, t(10));
        lifecycle.unsettle(t(20));
        // Last activity predates the un-settle: stays in the inbox for good.
        let stale = SettlementCandidate {
            branch_merged: true,
            ..candidate(&lifecycle, t(5))
        };
        assert_eq!(auto_settlement(stale, t(20) + 30 * DAY, &config), None);
        // New activity after the un-settle re-arms the rules.
        let active = SettlementCandidate {
            branch_merged: true,
            ..candidate(&lifecycle, t(25))
        };
        assert_eq!(
            auto_settlement(active, t(26), &config),
            Some(SettledReason::BranchMerged)
        );
    }

    #[test]
    fn config_defaults_survive_a_missing_or_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lifecycle.json");
        assert_eq!(
            LifecycleConfig::load_from(&path),
            LifecycleConfig::default()
        );
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            LifecycleConfig::load_from(&path),
            LifecycleConfig::default()
        );
        std::fs::write(&path, r#"{"auto_settle_after_days": 3}"#).unwrap();
        assert_eq!(
            LifecycleConfig::load_from(&path),
            LifecycleConfig {
                auto_settle_after_days: 3,
                auto_settle_on_merge: true,
            }
        );
    }
}
