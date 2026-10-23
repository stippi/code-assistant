//! The architecture overview of a session's project (its effective path,
//! so the overview follows a worktree switch). See [`crate::architecture`].

use super::*;
use crate::architecture::{ArchitectureOverview, ComponentSummary};

impl SessionService {
    /// Scan the session's project: its components, their dependencies and
    /// the cached summaries that apply. Asks no model.
    pub async fn architecture_overview(&self, session_id: String) -> Result<ArchitectureOverview> {
        self.call_io(move |ctx| async move {
            let root = {
                let manager = ctx.manager.lock().await;
                session_effective_path(&manager, &session_id)?
            };
            let analyzer = ctx.architecture.clone();
            tokio::task::spawn_blocking(move || analyzer.scan(&root))
                .await
                .context("Architecture scan was aborted")?
        })
        .await
    }

    /// Explain component `component_id` of the session's project: from the
    /// cache when its digest is unchanged, else with one model call.
    /// `force` asks the model regardless.
    pub async fn architecture_summary(
        &self,
        session_id: String,
        component_id: String,
        force: bool,
    ) -> Result<ComponentSummary> {
        self.call_io(move |ctx| async move {
            let root = {
                let manager = ctx.manager.lock().await;
                session_effective_path(&manager, &session_id)?
            };
            ctx.architecture
                .summarize(&root, &component_id, force)
                .await
        })
        .await
    }
}
