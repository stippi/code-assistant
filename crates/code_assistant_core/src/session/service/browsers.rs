//! A session's browsers through the service, for a browser panel (see
//! [`crate::session::browsers`]).

use super::*;
use crate::session::browsers::{BrowserEntry, BrowserKey, BrowserView, SessionBrowsers};

impl SessionService {
    /// The session's browsers now; from then on every change is published
    /// as [`UiEvent::BrowsersChanged`]. An unloaded session has none.
    pub async fn watch_browsers(&self, session_id: String) -> Result<Vec<BrowserEntry>> {
        self.call_io(move |ctx| async move {
            let Some(browsers) = session_browsers(&ctx, &session_id).await else {
                return Ok(Vec::new());
            };
            browsers.publish_to(ctx.events.clone(), session_id);
            Ok(browsers.listing())
        })
        .await
    }

    /// Watch tab `tab_id` (the active tab if `None`) of a session's browser,
    /// frames at most `max_size` large. The browser stays open while the
    /// view lives.
    pub async fn browser_view(
        &self,
        session_id: String,
        key: BrowserKey,
        tab_id: Option<String>,
        max_size: (u32, u32),
    ) -> Result<BrowserView> {
        self.call_io(move |ctx| async move {
            session_browsers(&ctx, &session_id)
                .await
                .ok_or_else(|| anyhow!("Session {session_id} not loaded"))?
                .view(&key, tab_id.as_deref(), max_size)
        })
        .await
    }

    /// Hand a browser to the user (`true`) or back to the agent. While the
    /// user has it, the agent's browser tools refuse to act on it.
    pub async fn set_browser_control(
        &self,
        session_id: String,
        key: BrowserKey,
        user: bool,
    ) -> Result<()> {
        self.call_control(move |ctx| async move {
            session_browsers(&ctx, &session_id)
                .await
                .ok_or_else(|| anyhow!("Session {session_id} not loaded"))?
                .set_user_control(&key, user)
        })
        .await
    }
}

/// The browsers of the session `session_id`; for a running sub-agent's
/// transcript, the sub-agent's.
async fn session_browsers(ctx: &ServiceCtx, session_id: &str) -> Option<Arc<SessionBrowsers>> {
    let manager = ctx.manager.lock().await;
    let sub_agent = crate::persistence::layout::sub_agent_parent(session_id)
        .and_then(|(parent, _)| manager.get_session(parent))
        .and_then(|parent| parent.browsers.transcript(session_id));
    sub_agent.or_else(|| {
        manager
            .get_session(session_id)
            .map(|instance| instance.browsers.clone())
    })
}
