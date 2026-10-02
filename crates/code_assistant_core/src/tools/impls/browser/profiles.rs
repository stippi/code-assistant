//! Profiles: closing a profile's browser, the human-in-the-loop login, and
//! listing the persistent profiles on disk.

use super::{BrowserOutput, DEFAULT_PROFILE, Target, browser_tool, launch_config_for, spec};
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use crate::tools::services::ToolServicesAccess;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tools_core::permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
};
use web::{BrowserSession, BrowserSessionManager, BrowserTimeout, Tab};

// ---------------------------------------------------------------------------
// browser_close
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct CloseInput {
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserCloseTool;

impl BrowserCloseTool {
    fn make_spec() -> ToolSpec {
        let mut spec = spec(
            "browser_close",
            "Close a profile's browser with all its tabs, flushing a persistent profile's \
             session to disk.",
            json!({"type": "object", "properties": {}}),
            false,
            "Closing browser",
        );
        if let Some(props) = spec.parameters_schema["properties"].as_object_mut() {
            props.remove("tab_id");
        }
        spec
    }
}

browser_tool!(BrowserCloseTool, CloseInput, close);

pub(crate) async fn close(manager: &BrowserSessionManager, input: &CloseInput) -> BrowserOutput {
    let profile = input.target.profile();
    match manager.remove_by_label(profile) {
        Some(session) => {
            session.close().await;
            BrowserOutput {
                profile: profile.to_string(),
                text: format!("Closed the browser of profile '{profile}'."),
                ..Default::default()
            }
        }
        None => BrowserOutput::failure(profile, "No browser is open for this profile."),
    }
}

// ---------------------------------------------------------------------------
// browser_login — human-in-the-loop login handoff
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct BrowserLoginInput {
    pub url: String,
    pub profile: String,
}

pub struct BrowserLoginTool;

/// Navigate, tolerating a load event that never comes: a slow portal page
/// must not fail a login the user already completed.
async fn navigate_tolerating_slow_load(tab: &Tab, url: &str) -> Result<()> {
    match tab.navigate(url).await {
        Err(e) if e.downcast_ref::<BrowserTimeout>().is_none() => Err(e),
        _ => Ok(()),
    }
}

/// The handoff itself, factored out so tests can drive it headlessly. In
/// production `headful` is always true: a visible window opens, the human logs
/// in, and only their approval lets the agent continue in that same
/// authenticated session.
async fn login_handoff(
    manager: &BrowserSessionManager,
    handler: &dyn PermissionMediator,
    tool_id: Option<&str>,
    profile: &str,
    url: &str,
    headful: bool,
) -> Result<BrowserOutput> {
    // A login needs a fresh window: replace any existing (possibly headless)
    // session for this profile.
    if let Some(existing) = manager.remove_by_label(profile) {
        existing.close().await;
    }
    let session = match BrowserSession::open(launch_config_for(profile, headful), profile).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            return Ok(BrowserOutput::failure(
                profile,
                format!("Failed to open browser: {e}"),
            ));
        }
    };
    let navigated = match session.active_tab() {
        Ok(tab) => navigate_tolerating_slow_load(&tab, url).await,
        Err(e) => Err(e),
    };
    if let Err(e) = navigated {
        session.close().await;
        return Ok(BrowserOutput::failure(
            profile,
            format!("Navigation failed: {e}"),
        ));
    }

    // Pause for the human to log in, then resume on approval. This travels the
    // same seam as any other permission prompt (TUI prompt / Telegram keyboard).
    let params = json!({
        "action": "browser_login",
        "profile": profile,
        "url": url,
        "instructions": "A browser window has opened. Log in there (password, 2FA, \
                         certificate as needed), then approve to let the agent continue.",
    });
    let decision = handler
        .request_permission(PermissionRequest {
            tool_id,
            tool_name: "browser_login",
            reason: PermissionRequestReason::ToolInvocation { params: &params },
        })
        .await?;

    match decision {
        PermissionDecision::Denied => {
            session.close().await;
            Ok(BrowserOutput::failure(
                profile,
                "User declined the login handoff.",
            ))
        }
        PermissionDecision::GrantedOnce
        | PermissionDecision::GrantedSession
        | PermissionDecision::GrantedPersistent => {
            // Swap the visible login window for a headless browser on the same
            // profile, carrying the login across. The agent then browses in the
            // background, and the user can close the login window without killing
            // the session.
            match finalize_login_headless(profile, session, url, headful).await {
                Ok(headless) => {
                    manager.register(headless.clone(), profile);
                    // Note the login so a later session can discover it via
                    // browser_profiles instead of asking the user to log in again.
                    record_login_in(&profiles_root(), profile, url);
                    let (url, title) = match headless.active_tab() {
                        Ok(tab) => tab.location().await,
                        Err(_) => Default::default(),
                    };
                    Ok(BrowserOutput {
                        profile: profile.to_string(),
                        text: format!(
                            "Logged in; profile '{profile}' now browses in the background.\n\
                             {url}\nTitle: {title}"
                        ),
                        ..Default::default()
                    })
                }
                Err(e) => Ok(BrowserOutput::failure(
                    profile,
                    format!("Login succeeded but switching to a background browser failed: {e}"),
                )),
            }
        }
    }
}

/// Page shown briefly in the login window after a successful handoff, so the
/// user sees it worked and knows the window is safe to close.
const LOGIN_SUCCESS_PAGE: &str = "data:text/html,<html><body style='font-family:sans-serif;padding:2rem'>\
     <h2>&#9989; Login successful</h2>\
     <p>You can close this window &mdash; the agent now continues in the background.</p>\
     </body></html>";

/// After a granted login, replace the visible headful browser with a headless
/// one on the same profile, transferring the full cookie jar (including
/// in-memory session cookies a disk flush would drop) so the login survives.
/// Chrome locks the profile dir, so the headful window must fully close before
/// the headless one can start.
async fn finalize_login_headless(
    profile: &str,
    headful: Arc<BrowserSession>,
    url: &str,
    was_headful: bool,
) -> Result<Arc<BrowserSession>> {
    // Capture the jar while the authenticated window is still alive.
    let cookies = headful.export_cookies().await.unwrap_or_default();
    // Reassure the user in the visible window, give them a moment to read it,
    // then close (releasing the profile-dir lock).
    if let Ok(tab) = headful.active_tab() {
        let _ = tab.navigate(LOGIN_SUCCESS_PAGE).await;
    }
    if was_headful {
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    headful.close().await;

    // Relaunch the same profile headless and restore the login.
    let headless =
        Arc::new(BrowserSession::open(launch_config_for(profile, false), profile).await?);
    let tab = headless.active_tab()?;
    if url.starts_with("http") {
        // Land on an http origin so cookies can be set, inject the jar, then
        // reload so the (now present) session cookies take effect.
        let _ = tab.navigate(url).await;
        let _ = tab.import_cookies(cookies).await;
    }
    navigate_tolerating_slow_load(&tab, url).await?;
    Ok(headless)
}

#[async_trait::async_trait]
impl Tool for BrowserLoginTool {
    type Input = BrowserLoginInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        let caps = [
            capabilities::READ_ONLY,
            capabilities::SCOPE_AGENT,
            capabilities::SCOPE_AGENT_DIFF,
            capabilities::SCOPE_SUBAGENT_DEFAULT,
            capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
        ];
        ToolSpec {
            name: "browser_login".into(),
            description: concat!(
                "Log in to a website AS THE USER without ever seeing their credentials. ",
                "Opens a VISIBLE browser window on the named persistent profile at the login ",
                "URL, then pauses until the user has logged in there (password, 2FA, ",
                "certificate) and approves. The login is saved under the profile for reuse.\n",
                "Tell the user what you are doing before calling this. Afterwards pass the same ",
                "`profile` to the browser tools."
            )
            .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "Login page URL"},
                    "profile": {
                        "type": "string",
                        "description": "Persistent profile name to store the login under (e.g. \"elster\")"
                    }
                },
                "required": ["url", "profile"]
            }),
            annotations: Some(json!({"readOnlyHint": true, "openWorldHint": true})),
            capabilities: ToolSpec::capabilities(&caps),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Logging in at {url}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profile = input.profile.clone();
        // The handoff needs a frontend that can prompt the human.
        let Some(handler) = context.permission_handler else {
            return Ok(BrowserOutput::failure(
                &profile,
                "Login handoff needs an interactive frontend, which this context does not have.",
            ));
        };
        let tool_id = context.tool_id.clone();
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(&profile));
        };
        login_handoff(
            manager,
            handler,
            tool_id.as_deref(),
            &profile,
            &input.url,
            true,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// browser_profiles — discover existing profiles and their last login
// ---------------------------------------------------------------------------

/// The directory holding all persistent browser profiles and their sidecar
/// metadata files (`<config_dir>/browser-profiles`).
pub(crate) fn profiles_root() -> PathBuf {
    crate::config_dir::config_dir().join("browser-profiles")
}

/// Sanitize a profile name to a filesystem-safe token — the same rule the launch
/// path uses, so a profile's dir and its `<name>.meta.json` sidecar agree.
pub(crate) fn sanitize_profile(profile: &str) -> String {
    profile
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Sidecar metadata recorded next to a persistent profile after a login. Kept
/// *beside* the profile dir (not inside it) so Chrome's user-data-dir stays
/// pristine and there is no central index to race on across sessions.
#[derive(Debug, Serialize, Deserialize)]
struct ProfileMeta {
    /// The login URL last used for this profile.
    url: String,
    /// When the last login handoff succeeded (unix seconds).
    logged_in_at_unix: i64,
}

fn meta_path_in(root: &Path, profile: &str) -> PathBuf {
    root.join(format!("{}.meta.json", sanitize_profile(profile)))
}

/// Record a successful login for `profile`. Best-effort — a metadata write must
/// never fail a login — and skipped for the ephemeral default profile, which
/// has no persistent dir.
fn record_login_in(root: &Path, profile: &str, url: &str) {
    if profile == DEFAULT_PROFILE {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let meta = ProfileMeta {
        url: url.to_string(),
        logged_in_at_unix: now,
    };
    let _ = std::fs::create_dir_all(root);
    if let Ok(json) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(meta_path_in(root, profile), json);
    }
}

fn read_meta_in(root: &Path, profile: &str) -> Option<ProfileMeta> {
    let data = std::fs::read_to_string(meta_path_in(root, profile)).ok()?;
    serde_json::from_str(&data).ok()
}

/// List the persistent profiles on disk (subdirectories of the root), each with
/// its login metadata if recorded, sorted by name.
fn list_profiles_in(root: &Path) -> Vec<(String, Option<ProfileMeta>)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Option<ProfileMeta>)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .map(|name| {
            let meta = read_meta_in(root, &name);
            (name, meta)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Render a duration as a coarse, human-friendly "N units ago", so the model can
/// judge at a glance whether a login is likely still fresh.
fn humanize_ago(seconds: i64) -> String {
    let s = seconds.max(0);
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    if s < MIN {
        return "just now".to_string();
    }
    let (n, unit) = if s < HOUR {
        (s / MIN, "minute")
    } else if s < DAY {
        (s / HOUR, "hour")
    } else if s < WEEK {
        (s / DAY, "day")
    } else if s < MONTH {
        (s / WEEK, "week")
    } else if s < YEAR {
        (s / MONTH, "month")
    } else {
        (s / YEAR, "year")
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Best-effort host extraction for display, e.g. `https://x.de/a` → `x.de`.
fn host_of(url: &str) -> String {
    let after = url.split("://").nth(1).unwrap_or(url);
    let host = after.split('/').next().unwrap_or(after);
    host.rsplit('@').next().unwrap_or(host).to_string()
}

/// Seconds elapsed since a recorded unix timestamp, floored at zero.
fn seconds_since(unix: i64) -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (now - unix).max(0)
}

#[derive(Deserialize, Serialize, Default)]
pub struct BrowserProfilesInput {}

#[derive(Serialize, Deserialize)]
pub struct BrowserProfilesOutput {
    text: String,
}

impl Render for BrowserProfilesOutput {
    fn status(&self) -> String {
        "Listed browser profiles".to_string()
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        self.text.clone()
    }
}

impl ToolResult for BrowserProfilesOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct BrowserProfilesTool;

#[async_trait::async_trait]
impl Tool for BrowserProfilesTool {
    type Input = BrowserProfilesInput;
    type Output = BrowserProfilesOutput;

    fn spec(&self) -> ToolSpec {
        let caps = [
            capabilities::READ_ONLY,
            capabilities::SCOPE_AGENT,
            capabilities::SCOPE_AGENT_DIFF,
            capabilities::SCOPE_SUBAGENT_DEFAULT,
            capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
        ];
        ToolSpec {
            name: "browser_profiles".into(),
            description: concat!(
                "List the browser profiles that already exist on disk, so you can reuse an ",
                "existing login instead of asking the user to log in again. Each persistent ",
                "profile shows how long ago it was LAST logged in and to which site. That is the ",
                "last recorded login, not a guarantee it is still valid — verify by navigating. ",
                "\"default\" is the ephemeral throwaway browser (no persisted login). To use a ",
                "profile pass it as `profile` to the browser tools; to create one use ",
                "browser_login."
            )
            .into(),
            parameters_schema: json!({ "type": "object", "properties": {} }),
            annotations: Some(json!({"readOnlyHint": true})),
            capabilities: ToolSpec::capabilities(&caps),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Listing browser profiles"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        _input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profiles = list_profiles_in(&profiles_root());
        let manager = context.browser_sessions();

        let mut lines = vec![
            "Profiles:".to_string(),
            "  default — ephemeral (throwaway)".to_string(),
        ];
        for (name, meta) in &profiles {
            let open = manager
                .map(|m| m.get_by_label(name).is_some())
                .unwrap_or(false);
            let open_suffix = if open { " · open" } else { "" };
            let detail = match meta {
                Some(m) => format!(
                    "last login: {}, {} (verify by navigating)",
                    host_of(&m.url),
                    humanize_ago(seconds_since(m.logged_in_at_unix)),
                ),
                None => "no login recorded yet".to_string(),
            };
            lines.push(format!("  {name} — persistent · {detail}{open_suffix}"));
        }
        if profiles.is_empty() {
            lines.push(String::new());
            lines.push(
                "No persistent profiles yet. Use browser_login to create one (it persists the \
                 login for reuse)."
                    .to_string(),
            );
        }
        Ok(BrowserProfilesOutput {
            text: lines.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;

    const DEMO_PAGE: &str =
        "<html><head><title>Login Demo</title></head><body><h1>Welcome</h1></body></html>";

    #[test]
    fn humanize_ago_rounds_coarsely() {
        assert_eq!(humanize_ago(0), "just now");
        assert_eq!(humanize_ago(59), "just now");
        assert_eq!(humanize_ago(60), "1 minute ago");
        assert_eq!(humanize_ago(3 * 60), "3 minutes ago");
        assert_eq!(humanize_ago(3600), "1 hour ago");
        assert_eq!(humanize_ago(5 * 3600), "5 hours ago");
        assert_eq!(humanize_ago(24 * 3600), "1 day ago");
        assert_eq!(humanize_ago(3 * 24 * 3600), "3 days ago");
        assert_eq!(humanize_ago(10 * 24 * 3600), "1 week ago");
        assert_eq!(humanize_ago(40 * 24 * 3600), "1 month ago");
        assert_eq!(humanize_ago(400 * 24 * 3600), "1 year ago");
        // Never negative, even if a clock skew makes "now" earlier.
        assert_eq!(humanize_ago(-500), "just now");
    }

    #[test]
    fn host_of_extracts_display_host() {
        assert_eq!(
            host_of("https://www.elster.de/eportal/start"),
            "www.elster.de"
        );
        assert_eq!(host_of("http://x.de"), "x.de");
        assert_eq!(host_of("elster.de/path"), "elster.de");
        assert_eq!(host_of("https://user@host.de/x"), "host.de");
    }

    #[test]
    fn profiles_are_listed_with_recorded_login() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Two profile dirs; only one has a recorded login.
        std::fs::create_dir_all(root.join("elster")).unwrap();
        std::fs::create_dir_all(root.join("fresh")).unwrap();
        record_login_in(root, "elster", "https://www.elster.de/eportal");

        let listed = list_profiles_in(root);
        let names: Vec<&str> = listed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["elster", "fresh"], "listed and sorted by name");

        let meta = listed
            .iter()
            .find(|(n, _)| n == "elster")
            .and_then(|(_, m)| m.as_ref())
            .expect("elster has a recorded login");
        assert_eq!(host_of(&meta.url), "www.elster.de");

        let fresh = listed.iter().find(|(n, _)| n == "fresh").unwrap();
        assert!(fresh.1.is_none(), "fresh profile has no login recorded");
    }

    #[test]
    fn record_login_skips_ephemeral_default() {
        let tmp = tempfile::tempdir().unwrap();
        record_login_in(tmp.path(), DEFAULT_PROFILE, "https://x.de");
        assert!(
            read_meta_in(tmp.path(), DEFAULT_PROFILE).is_none(),
            "the throwaway default profile must not get a sidecar"
        );
    }

    #[tokio::test]
    async fn browser_profiles_lists_the_default_profile() -> Result<()> {
        // Regardless of what's on disk, the output always leads with the header
        // and the ever-present ephemeral default.
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut input = BrowserProfilesInput::default();
        let out = BrowserProfilesTool
            .execute(&mut context, &mut input)
            .await?;
        let text = out.render(&mut ResourcesTracker::default());
        assert!(text.starts_with("Profiles:"), "got: {text}");
        assert!(
            text.contains("default — ephemeral (throwaway)"),
            "got: {text}"
        );
        Ok(())
    }

    /// Mediator returning a fixed decision, standing in for the human at the
    /// browser. Lets us drive the handoff headlessly (no window popped).
    struct ScriptedMediator(PermissionDecision);

    #[async_trait::async_trait]
    impl PermissionMediator for ScriptedMediator {
        async fn request_permission(
            &self,
            _request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn login_handoff_grant_keeps_authenticated_session() -> Result<()> {
        let manager = BrowserSessionManager::new(4);
        let mediator = ScriptedMediator(PermissionDecision::GrantedOnce);
        // Ephemeral profile ("default") so the test touches no config dir.
        let out = login_handoff(
            &manager,
            &mediator,
            None,
            "default",
            &super::super::test_support::data_url(DEMO_PAGE),
            false,
        )
        .await?;
        assert!(out.error.is_none(), "grant error: {:?}", out.error);
        assert!(
            out.text.contains("Login Demo"),
            "authenticated page reported: {}",
            out.text
        );
        assert!(
            manager.get_by_label("default").is_some(),
            "session should be kept after approval"
        );
        manager.close_all().await;
        Ok(())
    }

    #[tokio::test]
    async fn login_handoff_deny_closes_and_reports() -> Result<()> {
        let manager = BrowserSessionManager::new(4);
        let mediator = ScriptedMediator(PermissionDecision::Denied);
        let out = login_handoff(
            &manager,
            &mediator,
            None,
            "default",
            &super::super::test_support::data_url(DEMO_PAGE),
            false,
        )
        .await?;
        assert!(out.error.unwrap().contains("declined"));
        assert!(
            manager.get_by_label("default").is_none(),
            "no session should remain after a denied handoff"
        );
        Ok(())
    }

    #[tokio::test]
    async fn browser_login_without_a_handler_is_a_clear_error() -> Result<()> {
        // No permission handler ⇒ no way to ask the human ⇒ graceful error,
        // and crucially no browser is launched.
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut input = BrowserLoginInput {
            url: super::super::test_support::data_url(DEMO_PAGE),
            profile: "elster".into(),
        };
        let out = BrowserLoginTool.execute(&mut context, &mut input).await?;
        assert!(out.error.unwrap().contains("interactive frontend"));
        Ok(())
    }
}
