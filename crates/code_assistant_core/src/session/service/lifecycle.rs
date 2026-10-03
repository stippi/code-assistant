//! Session lifecycle through the service: visits, manual settlement and the
//! automatic settlement sweep (see [`crate::session::lifecycle`]).

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use super::*;
use crate::session::lifecycle::{
    LifecycleConfig, SessionLifecycle, SettledReason, SettlementCandidate, auto_settlement,
    sessions_with_merged_branch,
};

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

    /// Apply the automatic settlement rules once and return what settled.
    /// Git lookups run outside the session lock so a slow repository never
    /// stalls other commands.
    pub async fn sweep_settlement(
        &self,
        config: &LifecycleConfig,
    ) -> Result<Vec<(String, SettledReason)>> {
        if config.auto_settle_after_days == 0 && !config.auto_settle_on_merge {
            return Ok(Vec::new());
        }
        let candidates = self
            .call(move |ctx| async move {
                let manager = ctx.manager.lock().await;
                manager.settlement_candidates()
            })
            .await?;
        let merged = if config.auto_settle_on_merge {
            sessions_with_merged_branch(&candidates).await
        } else {
            HashSet::new()
        };
        let now = SystemTime::now();
        let mut settled = Vec::new();
        for candidate in candidates {
            let decision = auto_settlement(
                SettlementCandidate {
                    lifecycle: &candidate.lifecycle,
                    updated_at: candidate.updated_at,
                    busy: candidate.busy,
                    branch_merged: merged.contains(&candidate.session_id),
                },
                now,
                config,
            );
            let Some(reason) = decision else {
                continue;
            };
            self.update_lifecycle(candidate.session_id.clone(), move |lifecycle| {
                lifecycle.settle(reason, now)
            })
            .await?;
            settled.push((candidate.session_id, reason));
        }
        Ok(settled)
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

        let settled = service.sweep_settlement(&config).await.unwrap();

        assert_eq!(settled, vec![(old.clone(), SettledReason::Inactivity)]);
        let lifecycles = service.list_session_lifecycles().await.unwrap();
        assert!(lifecycles[&old].is_settled());
        assert!(!lifecycles.contains_key(&fresh));
        // A second sweep changes nothing.
        assert!(service.sweep_settlement(&config).await.unwrap().is_empty());
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

        assert!(service.sweep_settlement(&config).await.unwrap().is_empty());
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

        assert!(service.sweep_settlement(&config).await.unwrap().is_empty());
    }
}
