//! Session lifecycle through the service: visits, manual settlement and the
//! automatic settlement sweep (see [`crate::session::lifecycle`]).

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use github::RepoRef;

use super::*;
use crate::pull_requests::github_repo;
use crate::session::lifecycle::{
    LifecycleConfig, SessionLifecycle, SettledReason, SettlementCandidate, auto_settlement,
    sessions_with_merged_branch,
};
use crate::session::pull_request::{BranchLookup, PullRequestSnapshot, read_branch_pull_requests};

impl SessionService {
    /// Every session's lifecycle record, keyed by session id.
    pub async fn list_session_lifecycles(&self) -> Result<HashMap<String, SessionLifecycle>> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            manager.session_lifecycles()
        })
        .await
    }

    /// The user looked at the session just now. [`Self::load_session`]
    /// records a visit on its own; frontends call this when the viewed
    /// session changes under their eyes, e.g. its agent finished.
    pub async fn mark_session_visited(&self, session_id: String) -> Result<()> {
        self.update_lifecycle(session_id, |lifecycle| lifecycle.visit(SystemTime::now()))
            .await
    }

    /// Move a session into the settled shelf by hand.
    pub async fn settle_session(&self, session_id: String) -> Result<()> {
        self.update_lifecycle(session_id, |lifecycle| {
            lifecycle.settle(SettledReason::Manual, SystemTime::now())
        })
        .await
    }

    /// Pull a session back into the inbox. Automatic settlement leaves it
    /// alone until it sees new activity.
    pub async fn unsettle_session(&self, session_id: String) -> Result<()> {
        self.update_lifecycle(session_id, |lifecycle| {
            lifecycle.unsettle(SystemTime::now())
        })
        .await
    }

    async fn update_lifecycle(
        &self,
        session_id: String,
        update: impl FnOnce(&mut SessionLifecycle) + Send + 'static,
    ) -> Result<()> {
        self.call_session(session_id.clone(), move |ctx| async move {
            let manager = ctx.manager.lock().await;
            manager.update_session_lifecycle(&session_id, update)?;
            Ok(())
        })
        .await
    }

    /// Refresh the pull request of every unsettled session on a branch, then
    /// apply the automatic settlement rules once; returns what settled. Git
    /// and GitHub are asked outside the session lock so a slow repository or
    /// host never stalls other commands.
    pub async fn sweep_lifecycle(
        &self,
        config: &LifecycleConfig,
    ) -> Result<Vec<(String, SettledReason)>> {
        // What needs no lookup settles in one write; the rest below.
        let config_for_batch = config.clone();
        let mut settled = self
            .call(move |ctx| async move {
                let manager = ctx.manager.lock().await;
                manager.settle_inactive(&config_for_batch, SystemTime::now())
            })
            .await?;
        let mut candidates = self
            .call(move |ctx| async move {
                let manager = ctx.manager.lock().await;
                manager.settlement_candidates()
            })
            .await?;

        // Pull requests: a merged one is the strongest merge signal.
        let mut repos: HashMap<PathBuf, Option<RepoRef>> = HashMap::new();
        let mut lookups: Vec<(usize, BranchLookup)> = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let (Some(branch), Some(root)) = (&candidate.branch, &candidate.repo_root) else {
                continue;
            };
            if candidate.busy {
                continue;
            }
            if !repos.contains_key(root) {
                repos.insert(root.clone(), github_repo(root.clone()).await);
            }
            let Some(repo) = repos[root].clone() else {
                continue;
            };
            let known = candidate
                .lifecycle
                .pull_request
                .as_ref()
                .map(|pr| pr.number);
            lookups.push((
                index,
                BranchLookup {
                    repo,
                    branch: branch.clone(),
                    known,
                },
            ));
        }
        let branches: Vec<BranchLookup> = lookups.iter().map(|(_, l)| l.clone()).collect();
        let reads = self
            .call_io(move |ctx| async move {
                let client = ctx.pull_requests.client();
                Ok(read_branch_pull_requests(&client, &branches).await)
            })
            .await?;
        for ((index, lookup), read) in lookups.into_iter().zip(reads) {
            let candidate = &mut candidates[index];
            let fetched = match read {
                Ok(fetched) => fetched,
                Err(e) => {
                    debug!(
                        "Lifecycle sweep: cannot read the pull request of {}: {e}",
                        lookup.branch
                    );
                    continue;
                }
            };
            if same_pull_request(candidate.lifecycle.pull_request.as_ref(), fetched.as_ref()) {
                continue;
            }
            let session_id = candidate.session_id.clone();
            let update = fetched.clone();
            self.update_lifecycle(session_id, move |lifecycle| lifecycle.pull_request = update)
                .await?;
            candidate.lifecycle.pull_request = fetched;
        }

        if config.auto_settle_after_days == 0 && !config.auto_settle_on_merge {
            return Ok(Vec::new());
        }
        let merged = if config.auto_settle_on_merge {
            sessions_with_merged_branch(&candidates).await
        } else {
            HashSet::new()
        };
        let now = SystemTime::now();
        for candidate in candidates {
            let decision = auto_settlement(
                SettlementCandidate {
                    lifecycle: &candidate.lifecycle,
                    updated_at: candidate.updated_at,
                    busy: candidate.busy,
                    branch_merged: merged.contains(&candidate.session_id)
                        || candidate.lifecycle.pull_request_merged(),
                },
                now,
                config,
            );
            let Some(reason) = decision else {
                continue;
            };
            let at = match reason {
                SettledReason::Inactivity => candidate.updated_at,
                _ => now,
            };
            self.update_lifecycle(candidate.session_id.clone(), move |lifecycle| {
                lifecycle.settle(reason, at)
            })
            .await?;
            settled.push((candidate.session_id, reason));
        }
        Ok(settled)
    }
}

/// Equal apart from when they were fetched.
fn same_pull_request(
    current: Option<&PullRequestSnapshot>,
    fetched: Option<&PullRequestSnapshot>,
) -> bool {
    match (current, fetched) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            PullRequestSnapshot {
                fetched_at: b.fetched_at,
                ..a.clone()
            } == *b
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_service_with_manager;
    use super::*;
    use crate::persistence::FileSessionPersistence;
    use crate::session::EventPayload;
    use std::time::Duration;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    async fn next_lifecycle_event(
        subscription: &mut crate::session::event_stream::Subscription,
    ) -> (String, SessionLifecycle) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), subscription.recv())
                .await
                .expect("no lifecycle event")
                .unwrap();
            if let EventPayload::Ui(UiEvent::UpdateSessionLifecycle {
                session_id,
                lifecycle,
            }) = event.payload
            {
                return (session_id, lifecycle);
            }
        }
    }

    #[tokio::test]
    async fn loading_a_session_records_a_visit() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service.load_session(id.clone(), None).await.unwrap();

        let (visited_id, lifecycle) = next_lifecycle_event(&mut subscription).await;
        assert_eq!(visited_id, id);
        assert!(lifecycle.last_visited_at.is_some());
        let listed = service.list_session_lifecycles().await.unwrap();
        assert_eq!(listed.get(&id), Some(&lifecycle));
    }

    #[tokio::test]
    async fn settling_and_unsettling_by_hand_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        service.settle_session(id.clone()).await.unwrap();
        let settled = service.list_session_lifecycles().await.unwrap();
        assert_eq!(
            settled[&id].settled.map(|s| s.reason),
            Some(SettledReason::Manual)
        );

        service.unsettle_session(id.clone()).await.unwrap();
        let active = service.list_session_lifecycles().await.unwrap();
        assert!(!active[&id].is_settled());
        assert!(active[&id].unsettled_at.is_some());
    }

    /// Age a session's last activity on disk, like a long-idle session.
    fn age_session(root: &std::path::Path, id: &str, by: Duration) {
        let mut persistence = FileSessionPersistence::new_with_root_dir(root.to_path_buf());
        persistence
            .update_entry(id, |session| {
                session.updated_at = SystemTime::now() - by;
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn the_sweep_settles_inactive_sessions_and_keeps_recent_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _manager) = test_service_with_manager(tmp.path());
        let old = service.create_session(None, None).await.unwrap();
        let fresh = service.create_session(None, None).await.unwrap();
        age_session(tmp.path(), &old, 15 * DAY);
        let config = LifecycleConfig {
            auto_settle_after_days: 14,
            auto_settle_on_merge: false,
        };

        let settled = service.sweep_lifecycle(&config).await.unwrap();

        assert_eq!(settled, vec![(old.clone(), SettledReason::Inactivity)]);
        let lifecycles = service.list_session_lifecycles().await.unwrap();
        assert!(lifecycles[&old].is_settled());
        assert!(!lifecycles.contains_key(&fresh));
        // A second sweep changes nothing.
        assert!(service.sweep_lifecycle(&config).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn inactive_sessions_settle_in_one_batch_dated_by_their_last_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let mut ids = Vec::new();
        for days in [20u32, 30, 40] {
            let id = service.create_session(None, None).await.unwrap();
            age_session(tmp.path(), &id, DAY * days);
            ids.push(id);
        }
        let fresh = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        let config = LifecycleConfig {
            auto_settle_after_days: 14,
            auto_settle_on_merge: false,
        };

        let settled = manager
            .lock()
            .await
            .settle_inactive(&config, SystemTime::now())
            .unwrap();

        assert_eq!(settled.len(), 3);
        let lifecycles = service.list_session_lifecycles().await.unwrap();
        let listed = service.list_sessions().await.unwrap();
        for id in &ids {
            let settlement = lifecycles[id].settled.unwrap();
            let updated_at = listed.iter().find(|s| &s.id == id).unwrap().updated_at;
            assert_eq!(settlement.reason, SettledReason::Inactivity);
            assert_eq!(settlement.at, updated_at);
        }
        assert!(!lifecycles.contains_key(&fresh));
        // One reload request instead of one event per session.
        let event = tokio::time::timeout(Duration::from_secs(2), subscription.recv())
            .await
            .expect("no event")
            .unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::Ui(UiEvent::RefreshChatList)
        ));
        assert!(event.session_id.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(200), subscription.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_unsettled_session_is_left_alone_by_the_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();
        age_session(tmp.path(), &id, 30 * DAY);
        service.settle_session(id.clone()).await.unwrap();
        service.unsettle_session(id.clone()).await.unwrap();
        let config = LifecycleConfig {
            auto_settle_after_days: 14,
            auto_settle_on_merge: false,
        };

        assert!(service.sweep_lifecycle(&config).await.unwrap().is_empty());
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository on `main` with a `feature/x` branch checked out.
    fn repo_on_feature_branch(dir: &std::path::Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t.t"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(dir, &["checkout", "-q", "-b", "feature/x"]);
    }

    #[tokio::test]
    async fn a_run_remembers_the_branch_checked_out_in_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_on_feature_branch(&repo);
        let (service, manager) = test_service_with_manager(tmp.path());
        let config = crate::session::SessionConfig {
            init_path: Some(repo.clone()),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        service.load_session(id.clone(), None).await.unwrap();

        let observed = manager.lock().await.record_observed_branch(&id).unwrap();

        let root = observed
            .as_ref()
            .map(|(root, _)| root.canonicalize().unwrap());
        assert_eq!(root, Some(repo.canonicalize().unwrap()));
        assert_eq!(
            observed.map(|(_, branch)| branch).as_deref(),
            Some("feature/x")
        );
        let listed = service.list_sessions().await.unwrap();
        let session = listed.iter().find(|s| s.id == id).unwrap();
        assert_eq!(session.branch.as_deref(), Some("feature/x"));
    }

    #[tokio::test]
    async fn a_run_on_the_base_branch_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_on_feature_branch(&repo);
        git(&repo, &["checkout", "-q", "main"]);
        let (service, manager) = test_service_with_manager(tmp.path());
        let config = crate::session::SessionConfig {
            init_path: Some(repo.clone()),
            ..crate::session::SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();
        service.load_session(id.clone(), None).await.unwrap();

        assert!(
            manager
                .lock()
                .await
                .record_observed_branch(&id)
                .unwrap()
                .is_none()
        );
        let listed = service.list_sessions().await.unwrap();
        assert_eq!(listed.iter().find(|s| s.id == id).unwrap().branch, None);
    }

    #[tokio::test]
    async fn the_sweep_reads_pull_requests_through_github_and_settles_a_merged_one() {
        use crate::session::pull_request::tests::{head_nodes, summary_node};
        use crate::session::pull_request::{PullRequestState, ReviewDecision};
        use github::fake::FakeTransport;
        use serde_json::json;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        repo_on_feature_branch(&repo);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&repo, &["checkout", "-q", "-b", "feature/y", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "y"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/stippi/code-assistant.git",
            ],
        );
        let fake = FakeTransport::new();
        fake.on_graphql("PullRequestSummary")
            .with_variables(json!({"number_0": 7}))
            .data(
                json!({"a0_repository": {"pullRequest": summary_node(7, "feature/x", "MERGED")}}),
            );
        fake.on_graphql("PullRequestsByHead")
            .with_variables(json!({"head": "feature/y"}))
            .data(head_nodes("feature/y", &[8]));
        fake.on_graphql("PullRequestSummary")
            .with_variables(json!({"number_0": 8}))
            .data(json!({"a0_repository": {"pullRequest": summary_node(8, "feature/y", "OPEN")}}));
        let source = crate::pull_requests::PullRequestSource {
            client: fake.client(),
            snapshot_dir: tmp.path().join("pull-requests"),
        };
        let (service, manager) = super::super::tests::test_service_with_pull_requests(
            &tmp.path().join("sessions"),
            source,
        );
        let mut ids = Vec::new();
        for branch in ["feature/x", "feature/y"] {
            let config = crate::session::SessionConfig {
                init_path: Some(repo.clone()),
                initial_project: "pull-request-sweep-test".into(),
                branch: Some(branch.into()),
                ..crate::session::SessionConfig::default()
            };
            let id = service
                .create_session_with_config(None, config, None)
                .await
                .unwrap();
            service.load_session(id.clone(), None).await.unwrap();
            ids.push(id);
        }
        manager
            .lock()
            .await
            .update_session_lifecycle(&ids[0], |lifecycle| {
                lifecycle.pull_request = Some(PullRequestSnapshot {
                    number: 7,
                    url: String::new(),
                    title: "PR 7".into(),
                    state: PullRequestState::Open,
                    review: None,
                    checks: None,
                    fetched_at: SystemTime::UNIX_EPOCH,
                })
            })
            .unwrap();
        let config = LifecycleConfig {
            auto_settle_after_days: 0,
            auto_settle_on_merge: true,
        };

        let settled = service.sweep_lifecycle(&config).await.unwrap();

        assert_eq!(settled, vec![(ids[0].clone(), SettledReason::BranchMerged)]);
        let lifecycles = service.list_session_lifecycles().await.unwrap();
        let merged = lifecycles[&ids[0]].pull_request.as_ref().unwrap();
        assert_eq!(merged.state, PullRequestState::Merged);
        let found = lifecycles[&ids[1]].pull_request.as_ref().unwrap();
        assert_eq!((found.number, found.state), (8, PullRequestState::Open));
        assert_eq!(found.review, Some(ReviewDecision::Approved));
        // Known pull requests in one batch, the rest by branch.
        assert_eq!(
            fake.operations(),
            [
                "PullRequestSummary",
                "PullRequestsByHead",
                "PullRequestSummary"
            ]
        );
    }

    #[tokio::test]
    async fn the_sweep_does_nothing_when_both_rules_are_off() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();
        age_session(tmp.path(), &id, 400 * DAY);
        let config = LifecycleConfig {
            auto_settle_after_days: 0,
            auto_settle_on_merge: false,
        };

        assert!(service.sweep_lifecycle(&config).await.unwrap().is_empty());
    }
}
