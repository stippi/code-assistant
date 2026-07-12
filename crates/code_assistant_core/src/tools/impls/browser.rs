//! Browser agency tools.
//!
//! These drive a real browser across tool calls so the agent can operate web
//! software: test an app under development, or act on a portal under the user's
//! identity. State lives in the session-scoped `web::BrowserSessionManager`
//! (see [`crate::tools::services`]); one live browser is kept per **profile**
//! name, so the model addresses "the elster browser" by name instead of
//! juggling session ids.
//!
//! - `browser_navigate` — open-or-reuse a profile's browser and go to a URL.
//! - `browser_read` — re-observe the current page without acting.
//! - `browser_act` — click / type / press / wait, a sequence per call.
//! - `browser_close` — close a profile's browser (flushing a persistent one).
//! - `browser_login` — headful human-in-the-loop login handoff: opens a visible
//!   window, pauses on the `PermissionMediator` seam for the user to log in,
//!   resumes authenticated. The model never sees credentials.
//!
//! Every tool returns a screenshot (the model's eyes, via `render_images`)
//! plus the page's url/title/text.

use crate::tools::core::{
    capabilities, ImageData, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::services::ToolServicesAccess;
use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tools_core::permissions::{
    PermissionDecision, PermissionMediator, PermissionRequest, PermissionRequestReason,
};
use web::{
    BrowserLaunchConfig, BrowserProfile, BrowserSession, BrowserSessionManager, PageObservation,
};

/// The profile label used when the model does not name one: a single reusable
/// ephemeral (throwaway) browser.
pub(crate) const DEFAULT_PROFILE: &str = "default";

/// Resolve a profile name to a launch config. The reserved `"default"` name (or
/// `None`) is an ephemeral throwaway browser; any other name is a persistent
/// profile under `<config_dir>/browser-profiles/<name>`, so a login can be
/// reused across runs.
pub(crate) fn launch_config_for(profile: &str, headful: bool) -> BrowserLaunchConfig {
    if profile == DEFAULT_PROFILE {
        return BrowserLaunchConfig {
            profile: BrowserProfile::Ephemeral,
            headful,
        };
    }
    let sanitized: String = profile
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = crate::config_dir::config_dir()
        .join("browser-profiles")
        .join(sanitized);
    BrowserLaunchConfig {
        profile: BrowserProfile::Persistent(dir),
        headful,
    }
}

/// Get the live browser for `profile`, opening one if none exists yet.
pub(crate) async fn get_or_open(
    manager: &BrowserSessionManager,
    profile: &str,
    headful: bool,
) -> Result<Arc<BrowserSession>> {
    if let Some(session) = manager.get_by_label(profile) {
        return Ok(session);
    }
    let session =
        Arc::new(BrowserSession::open(launch_config_for(profile, headful), profile).await?);
    manager.register(session.clone(), profile);
    Ok(session)
}

/// Shared output of every browser tool: what the page looks like now.
#[derive(Serialize, Deserialize)]
pub struct BrowserOutput {
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<PageObservation>,
    /// Base64 PNG screenshot, surfaced to the model as an image via
    /// `render_images`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BrowserOutput {
    fn failure(profile: &str, error: impl Into<String>) -> Self {
        Self {
            profile: profile.to_string(),
            observation: None,
            screenshot_base64: None,
            error: Some(error.into()),
        }
    }

    /// The standard output when this context has no browser registry.
    fn unavailable(profile: &str) -> Self {
        Self::failure(
            profile,
            "Browser tools are not available in this context (no browser session registry).",
        )
    }

    /// Observe + screenshot the session into a success output. Any capture
    /// error is folded into `error` rather than failing the whole tool call.
    async fn capture(profile: &str, session: &BrowserSession, full_page: bool) -> Self {
        // Let a navigation triggered by the preceding action settle first, so
        // the text (`observe`) and the screenshot show the same page rather
        // than racing a mid-transition document.
        session.settle().await;
        let observation = session.observe().await.ok();
        let screenshot_base64 = match session.screenshot(full_page).await {
            Ok(png) => Some(base64::engine::general_purpose::STANDARD.encode(png)),
            Err(_) => None,
        };
        Self {
            profile: profile.to_string(),
            observation,
            screenshot_base64,
            error: None,
        }
    }
}

impl Render for BrowserOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            return format!("Browser error: {e}");
        }
        match &self.observation {
            Some(obs) => format!("{} — {}", obs.url, obs.title),
            None => "Browser action completed".to_string(),
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Browser error: {e}");
        }
        let Some(obs) = &self.observation else {
            return "Browser action completed (no page observed).".to_string();
        };
        // Keep the textual dump bounded; the screenshot carries the visual
        // detail.
        let mut text = obs.text.trim().to_string();
        const MAX: usize = 4000;
        if text.len() > MAX {
            text.truncate(MAX);
            text.push_str("\n… (truncated; see screenshot for the rest)");
        }
        let mut out = format!(
            "Profile: {}\nURL: {}\nTitle: {}",
            self.profile, obs.url, obs.title
        );
        // Disclose the viewport size (CSS px) so the model can address px
        // coordinates; it can't read the true size off the resized screenshot.
        if obs.viewport_width > 0.0 && obs.viewport_height > 0.0 {
            out.push_str(&format!(
                "\nViewport: {}×{} (CSS px)",
                obs.viewport_width as i64, obs.viewport_height as i64
            ));
        }
        out.push_str("\n\n");
        out.push_str(&text);
        // List the actionable elements with their selectors, so the model can
        // target them directly with browser_act instead of guessing.
        if !obs.elements.is_empty() {
            out.push_str("\n\nInteractive elements:");
            for el in &obs.elements {
                if el.label.is_empty() {
                    out.push_str(&format!("\n  [{}] {}", el.role, el.selector));
                } else {
                    out.push_str(&format!(
                        "\n  [{}] \"{}\" → {}",
                        el.role, el.label, el.selector
                    ));
                }
            }
        }
        out
    }

    fn render_images(&self) -> Vec<ImageData> {
        // An error tool result must be text-only (Anthropic rejects images in a
        // tool_result with is_error=true). Some error paths still capture a
        // screenshot for context (e.g. browser_act showing where a sequence
        // stopped); drop it here so the result stays valid — the failing
        // step's text still explains what happened.
        if self.error.is_some() {
            return Vec::new();
        }
        self.screenshot_base64
            .iter()
            .map(|data| ImageData {
                media_type: "image/png".to_string(),
                base64_data: data.clone(),
            })
            .collect()
    }
}

impl ToolResult for BrowserOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Common scope tags for browser tools (agent + default sub-agents).
fn agent_scopes() -> Vec<&'static str> {
    vec![
        capabilities::SCOPE_AGENT,
        capabilities::SCOPE_AGENT_DIFF,
        capabilities::SCOPE_SUBAGENT_DEFAULT,
        capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
    ]
}

// ---------------------------------------------------------------------------
// browser_navigate
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct BrowserNavigateInput {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

pub struct BrowserNavigateTool;

#[async_trait::async_trait]
impl Tool for BrowserNavigateTool {
    type Input = BrowserNavigateInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        let mut caps = vec![capabilities::READ_ONLY];
        caps.extend(agent_scopes());
        caps.push(capabilities::SCOPE_SUBAGENT_READ_ONLY);
        ToolSpec {
            name: "browser_navigate".into(),
            description: concat!(
                "Open (or reuse) a browser and navigate to a URL, then return a screenshot ",
                "and the page's text. Use this to try out and inspect web software.\n",
                "Pass `profile` to use a persistent, named browser whose login/cookies ",
                "survive across runs (e.g. \"elster\"); omit it for a throwaway browser. ",
                "To log in to a persistent profile, use `browser_login` first."
            )
            .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "URL to navigate to"},
                    "profile": {
                        "type": "string",
                        "description": "Named persistent profile to reuse a login; omit for a throwaway browser"
                    }
                },
                "required": ["url"]
            }),
            annotations: Some(json!({"readOnlyHint": true, "openWorldHint": true})),
            capabilities: ToolSpec::capabilities(&caps),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Navigating to {url}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profile = input
            .profile
            .as_deref()
            .unwrap_or(DEFAULT_PROFILE)
            .to_string();
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(&profile));
        };
        let session = match get_or_open(manager, &profile, false).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(BrowserOutput::failure(
                    &profile,
                    format!("Failed to open browser: {e}"),
                ))
            }
        };
        if let Err(e) = session.navigate(&input.url).await {
            return Ok(BrowserOutput::failure(
                &profile,
                format!("Navigation failed: {e}"),
            ));
        }
        Ok(BrowserOutput::capture(&profile, &session, false).await)
    }
}

// ---------------------------------------------------------------------------
// browser_read
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct BrowserReadInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Capture the entire scrollable page instead of just the viewport.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_page: bool,
}

pub struct BrowserReadTool;

#[async_trait::async_trait]
impl Tool for BrowserReadTool {
    type Input = BrowserReadInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        let mut caps = vec![capabilities::READ_ONLY];
        caps.extend(agent_scopes());
        caps.push(capabilities::SCOPE_SUBAGENT_READ_ONLY);
        ToolSpec {
            name: "browser_read".into(),
            description: concat!(
                "Re-observe the current page of a browser profile without acting: returns a ",
                "fresh screenshot and the page text. Use after something on the page changes."
            )
            .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "profile": {"type": "string", "description": "Profile to read; omit for the throwaway browser"},
                    "full_page": {"type": "boolean", "description": "Capture the whole scrollable page instead of just the viewport"}
                }
            }),
            annotations: Some(json!({"readOnlyHint": true})),
            capabilities: ToolSpec::capabilities(&caps),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Reading browser page"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profile = input
            .profile
            .as_deref()
            .unwrap_or(DEFAULT_PROFILE)
            .to_string();
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(&profile));
        };
        match manager.get_by_label(&profile) {
            Some(session) => Ok(BrowserOutput::capture(&profile, &session, input.full_page).await),
            None => Ok(BrowserOutput::failure(
                &profile,
                "No open browser for this profile. Use browser_navigate first.",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// browser_act
// ---------------------------------------------------------------------------

/// One interaction step. A `browser_act` call runs a sequence of these.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    /// Click the first element matching the CSS selector.
    Click { selector: String },
    /// Focus a field and type text into it. Not for credentials — use
    /// `browser_login` for those.
    Type { selector: String, text: String },
    /// Press a key (e.g. "Enter") on the element matching the selector. The
    /// element is focused first. Omit `selector` to send the key to whatever is
    /// currently focused (e.g. arrow keys for a focused game canvas).
    Press {
        #[serde(default)]
        selector: Option<String>,
        key: String,
    },
    /// Wait until an element appears (or the timeout elapses).
    WaitFor {
        selector: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Scroll the page: with `selector`, scroll that element into view;
    /// otherwise scroll by `(dx, dy)` pixels (positive `dy` scrolls down).
    Scroll {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        dx: Option<f64>,
        #[serde(default)]
        dy: Option<f64>,
    },
    /// Click at a coordinate. Each value carries a CSS unit: `"40vw"`/`"50%"`
    /// (fraction of the viewport axis) or `"640px"` (exact CSS pixels — the
    /// viewport size is shown in a read). For canvas/WebGL surfaces and anything
    /// without a stable selector (games, maps, drag targets).
    ClickAt { x: String, y: String },
    /// Move the mouse to a coordinate (same unit rules as `click_at`) without
    /// clicking — drives hover states and canvas pointer-move handlers.
    MoveAt { x: String, y: String },
}

/// Which viewport axis a coordinate is measured against.
#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

/// Resolve a unit-bearing coordinate (`"40vw"`, `"50%"`, `"640px"`) to CSS
/// pixels along `axis`, given the viewport size. `vw`/`vh` are always width/
/// height; `%` follows the axis; `px` passes through. Typographic units
/// (`rem`/`em`) and bare numbers are rejected so the model states a groundable
/// unit. The result is clamped to the viewport so a click can't land off-page.
fn resolve_coord(value: &str, axis: Axis, vw: f64, vh: f64) -> Result<f64> {
    let s = value.trim();
    let split = s
        .find(|c: char| c.is_alphabetic() || c == '%')
        .unwrap_or(s.len());
    let (num_str, unit) = s.split_at(split);
    let num: f64 = num_str.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid coordinate '{value}' (expected e.g. \"40vw\", \"50%\", \"640px\")")
    })?;
    let px = match unit.trim() {
        "px" => num,
        "vw" => num / 100.0 * vw,
        "vh" => num / 100.0 * vh,
        "%" => match axis {
            Axis::X => num / 100.0 * vw,
            Axis::Y => num / 100.0 * vh,
        },
        "" => {
            return Err(anyhow::anyhow!(
                "coordinate '{value}' needs a unit — use vw/vh/%/px"
            ))
        }
        other => {
            return Err(anyhow::anyhow!(
                "unit '{other}' not supported for coordinates; use vw/vh/%/px"
            ))
        }
    };
    let max = match axis {
        Axis::X => vw,
        Axis::Y => vh,
    };
    Ok(px.clamp(0.0, max))
}

#[derive(Deserialize, Serialize)]
pub struct BrowserActInput {
    pub actions: Vec<BrowserAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

pub struct BrowserActTool;

impl BrowserActTool {
    async fn run_action(session: &BrowserSession, action: &BrowserAction) -> Result<()> {
        match action {
            BrowserAction::Click { selector } => session.click(selector).await,
            BrowserAction::Type { selector, text } => session.type_text(selector, text).await,
            BrowserAction::Press { selector, key } => match selector {
                Some(sel) => session.press_key(sel, key).await,
                None => session.press_key_global(key).await,
            },
            BrowserAction::WaitFor {
                selector,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(5000));
                let appeared = session.wait_for(selector, timeout).await?;
                if appeared {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("timed out waiting for '{selector}'"))
                }
            }
            BrowserAction::Scroll { selector, dx, dy } => {
                session
                    .scroll(selector.as_deref(), dx.unwrap_or(0.0), dy.unwrap_or(0.0))
                    .await
            }
            BrowserAction::ClickAt { x, y } => {
                let (vw, vh) = session.viewport_size().await?;
                let px = resolve_coord(x, Axis::X, vw, vh)?;
                let py = resolve_coord(y, Axis::Y, vw, vh)?;
                session.click_at(px, py).await
            }
            BrowserAction::MoveAt { x, y } => {
                let (vw, vh) = session.viewport_size().await?;
                let px = resolve_coord(x, Axis::X, vw, vh)?;
                let py = resolve_coord(y, Axis::Y, vw, vh)?;
                session.move_mouse(px, py).await
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for BrowserActTool {
    type Input = BrowserActInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_act".into(),
            description: concat!(
                "Interact with the current page of a browser profile: a sequence of ",
                "click / type / press / wait_for / scroll / click_at / move_at steps, executed ",
                "in order. Returns a screenshot and text of the resulting page.\n",
                "Each action is an object with one key: {\"click\": {\"selector\": \"#go\"}}, ",
                "{\"type\": {\"selector\": \"#user\", \"text\": \"hello\"}}, ",
                "{\"press\": {\"selector\": \"#user\", \"key\": \"Enter\"}} (omit selector to send ",
                "the key to whatever is focused, e.g. arrow keys for a game canvas), ",
                "{\"wait_for\": {\"selector\": \"#result\", \"timeout_ms\": 5000}}, ",
                "{\"scroll\": {\"dy\": 600}} (scroll down 600px) / {\"scroll\": {\"selector\": \"#footer\"}} ",
                "(scroll an element into view), ",
                "{\"click_at\": {\"x\": \"40vw\", \"y\": \"30vh\"}} (click at a coordinate, for ",
                "canvas/WebGL surfaces without selectors), or {\"move_at\": {\"x\": \"40vw\", ",
                "\"y\": \"30vh\"}} (move the mouse for hover/pointer-move).\n",
                "Coordinate values carry a CSS unit — think about which unit you mean: \"40vw\"/",
                "\"30vh\" or \"50%\" express a fraction of the viewport axis (robust — use these ",
                "when eyeballing from the screenshot); \"640px\" is exact CSS pixels (only when you ",
                "know the size — the read output shows the Viewport dimensions). rem/em are not ",
                "accepted. Prefer selectors when available (see the interactive-element list from a ",
                "read); use coordinates only for canvas/game surfaces. ",
                "Do not type passwords or 2FA codes here — use browser_login."
            )
            .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "actions": {
                        "type": "array",
                        "description": "Ordered interaction steps",
                        "items": {
                            "type": "object",
                            "properties": {
                                "click": {"type": "object", "properties": {"selector": {"type": "string"}}, "required": ["selector"]},
                                "type": {"type": "object", "properties": {"selector": {"type": "string"}, "text": {"type": "string"}}, "required": ["selector", "text"]},
                                "press": {"type": "object", "properties": {"selector": {"type": "string"}, "key": {"type": "string"}}, "required": ["key"]},
                                "wait_for": {"type": "object", "properties": {"selector": {"type": "string"}, "timeout_ms": {"type": "integer"}}, "required": ["selector"]},
                                "scroll": {"type": "object", "properties": {"selector": {"type": "string"}, "dx": {"type": "number"}, "dy": {"type": "number"}}},
                                "click_at": {"type": "object", "properties": {"x": {"type": "string", "description": "x coordinate with CSS unit: vw/% (of width) or px"}, "y": {"type": "string", "description": "y coordinate with CSS unit: vh/% (of height) or px"}}, "required": ["x", "y"]},
                                "move_at": {"type": "object", "properties": {"x": {"type": "string"}, "y": {"type": "string"}}, "required": ["x", "y"]}
                            }
                        }
                    },
                    "profile": {"type": "string", "description": "Profile to act on; omit for the throwaway browser"}
                },
                "required": ["actions"]
            }),
            annotations: Some(json!({"readOnlyHint": false})),
            capabilities: ToolSpec::capabilities(&agent_scopes()),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Interacting with the browser"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profile = input
            .profile
            .as_deref()
            .unwrap_or(DEFAULT_PROFILE)
            .to_string();
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(&profile));
        };
        let Some(session) = manager.get_by_label(&profile) else {
            return Ok(BrowserOutput::failure(
                &profile,
                "No open browser for this profile. Use browser_navigate first.",
            ));
        };

        for (i, action) in input.actions.iter().enumerate() {
            if let Err(e) = Self::run_action(&session, action).await {
                // Capture the page as it stands so the model can see where the
                // sequence stopped, but report the failing step.
                let mut out = BrowserOutput::capture(&profile, &session, false).await;
                out.error = Some(format!("Action {} failed: {e}", i + 1));
                return Ok(out);
            }
        }
        Ok(BrowserOutput::capture(&profile, &session, false).await)
    }
}

// ---------------------------------------------------------------------------
// browser_close
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct BrowserCloseInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

pub struct BrowserCloseTool;

#[async_trait::async_trait]
impl Tool for BrowserCloseTool {
    type Input = BrowserCloseInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_close".into(),
            description:
                "Close a browser profile's window, flushing a persistent profile's session to disk."
                    .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "profile": {"type": "string", "description": "Profile to close; omit for the throwaway browser"}
                }
            }),
            annotations: Some(json!({"readOnlyHint": false})),
            capabilities: ToolSpec::capabilities(&agent_scopes()),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Closing browser"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let profile = input
            .profile
            .as_deref()
            .unwrap_or(DEFAULT_PROFILE)
            .to_string();
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(&profile));
        };
        match manager.remove_by_label(&profile) {
            Some(session) => {
                session.close().await;
                Ok(BrowserOutput {
                    profile,
                    observation: None,
                    screenshot_base64: None,
                    error: None,
                })
            }
            None => Ok(BrowserOutput::failure(
                &profile,
                "No open browser for this profile.",
            )),
        }
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
            ))
        }
    };
    if let Err(e) = session.navigate(url).await {
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
        PermissionDecision::GrantedOnce | PermissionDecision::GrantedSession => {
            // Keep the now-authenticated session for reuse by the other tools.
            manager.register(session.clone(), profile);
            Ok(BrowserOutput::capture(profile, &session, false).await)
        }
    }
}

#[async_trait::async_trait]
impl Tool for BrowserLoginTool {
    type Input = BrowserLoginInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        let mut caps = vec![capabilities::READ_ONLY];
        caps.extend(agent_scopes());
        ToolSpec {
            name: "browser_login".into(),
            description: concat!(
                "Log in to a website AS THE USER without ever seeing their credentials. ",
                "Opens a VISIBLE browser window on the named persistent profile, navigates to ",
                "the login URL, then pauses and asks the user to complete the login (password, ",
                "2FA, certificate — whatever the site needs) in that window and approve. On ",
                "approval the agent continues in the same authenticated window, and the session ",
                "is saved under the profile for reuse.\n",
                "Tell the user what you are doing before calling this. Afterwards use the same ",
                "`profile` name with browser_navigate / browser_act."
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use tools_core::permissions::PermissionDecision;

    /// A self-contained page (base64 data URL, no server): a field, a button
    /// whose JS writes the typed value into a result span, and a title.
    fn demo_page_url() -> String {
        let html = concat!(
            "<html><head><title>Login Demo</title></head><body>",
            "<h1>Welcome</h1>",
            "<input id=\"user\">",
            "<button id=\"go\" onclick=\"document.getElementById('who').innerText='Hello '+document.getElementById('user').value\">Go</button>",
            "<span id=\"who\"></span>",
            "</body></html>"
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(html);
        format!("data:text/html;base64,{b64}")
    }

    #[test]
    fn launch_config_maps_default_to_ephemeral_and_names_to_persistent() {
        let default = launch_config_for(DEFAULT_PROFILE, false);
        assert!(matches!(default.profile, BrowserProfile::Ephemeral));

        let named = launch_config_for("elster", false);
        match named.profile {
            BrowserProfile::Persistent(path) => {
                assert_eq!(path.file_name().unwrap(), "elster");
                assert!(path.to_string_lossy().contains("browser-profiles"));
            }
            _ => panic!("named profile should be persistent"),
        }
    }

    #[test]
    fn launch_config_sanitizes_path_traversal_in_profile_names() {
        let named = launch_config_for("../evil name", false);
        let BrowserProfile::Persistent(path) = named.profile else {
            panic!("expected persistent");
        };
        let last = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!last.contains('/'), "no path separators: {last}");
        assert!(!last.contains(".."), "no traversal: {last}");
        assert_eq!(last, "___evil_name");
    }

    #[tokio::test]
    async fn navigate_act_read_close_round_trip() -> Result<()> {
        let mut fixture = ToolTestFixture::new().with_browser_sessions();

        // Navigate opens a browser and returns a screenshot + the page text.
        let mut context = fixture.context();
        let mut nav = BrowserNavigateInput {
            url: demo_page_url(),
            profile: None,
        };
        let out = BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        assert!(out.error.is_none(), "navigate error: {:?}", out.error);
        let obs = out.observation.as_ref().expect("observation");
        assert_eq!(obs.title, "Login Demo");
        assert!(obs.text.contains("Welcome"), "text: {}", obs.text);
        // The observation surfaces the actionable elements, and render() lists
        // them with their selectors so the model can target them directly.
        assert!(
            obs.elements.iter().any(|e| e.selector == "#go"),
            "should discover the submit button, got: {:?}",
            obs.elements
        );
        let rendered = out.render(&mut ResourcesTracker::default());
        assert!(
            rendered.contains("Interactive elements:") && rendered.contains("#go"),
            "render should list interactive elements, got:\n{rendered}"
        );
        assert_eq!(
            out.render_images().len(),
            1,
            "screenshot should be attached"
        );
        assert_eq!(out.render_images()[0].media_type, "image/png");

        // Act: type into the field, click the button (JS writes the result).
        let mut act = BrowserActInput {
            actions: vec![
                BrowserAction::Type {
                    selector: "#user".into(),
                    text: "stephan".into(),
                },
                BrowserAction::Click {
                    selector: "#go".into(),
                },
                BrowserAction::WaitFor {
                    selector: "#who".into(),
                    timeout_ms: Some(2000),
                },
            ],
            profile: None,
        };
        let out = BrowserActTool.execute(&mut context, &mut act).await?;
        assert!(out.error.is_none(), "act error: {:?}", out.error);

        // Read: the typed value round-tripped into the page.
        let mut read = BrowserReadInput {
            profile: None,
            full_page: false,
        };
        let out = BrowserReadTool.execute(&mut context, &mut read).await?;
        let obs = out.observation.as_ref().expect("observation");
        assert!(
            obs.text.contains("Hello stephan"),
            "expected typed value in page, got: {}",
            obs.text
        );

        // Close removes the session from the registry.
        let mut close = BrowserCloseInput { profile: None };
        let out = BrowserCloseTool.execute(&mut context, &mut close).await?;
        assert!(out.error.is_none(), "close error: {:?}", out.error);
        assert!(
            fixture
                .browser_sessions()
                .unwrap()
                .get_by_label("default")
                .is_none(),
            "closed session should be gone"
        );
        Ok(())
    }

    #[tokio::test]
    async fn scroll_moves_the_viewport_and_full_page_capture_works() -> Result<()> {
        // A page much taller than the viewport.
        let html = "<html><body style=\"height:5000px;margin:0\">\
                    <div id=\"top\">TOP</div>\
                    <div id=\"bottom\" style=\"position:absolute;top:4500px\">BOTTOM</div>\
                    </body></html>";
        let url = format!(
            "data:text/html;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(html)
        );

        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        {
            let mut context = fixture.context();
            let mut nav = BrowserNavigateInput { url, profile: None };
            BrowserNavigateTool.execute(&mut context, &mut nav).await?;

            // Scroll down by pixels.
            let mut act = BrowserActInput {
                actions: vec![BrowserAction::Scroll {
                    selector: None,
                    dx: None,
                    dy: Some(1200.0),
                }],
                profile: None,
            };
            let out = BrowserActTool.execute(&mut context, &mut act).await?;
            assert!(out.error.is_none(), "scroll error: {:?}", out.error);
        }

        let session = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap();

        // The viewport actually moved down.
        let y = session.eval("window.scrollY").await?;
        assert!(
            y.as_f64().unwrap_or(0.0) >= 900.0,
            "page should have scrolled down, scrollY={y}"
        );

        // Scrolling an element into view reaches the bottom element.
        session.scroll(Some("#bottom"), 0.0, 0.0).await?;
        let y2 = session.eval("window.scrollY").await?;
        assert!(
            y2.as_f64().unwrap_or(0.0) > 3000.0,
            "scroll-into-view should reach the bottom element, scrollY={y2}"
        );

        // A full-page screenshot succeeds (captures beyond the viewport).
        let png = session.screenshot(true).await?;
        assert!(png.starts_with(b"\x89PNG"), "full-page screenshot is a PNG");
        Ok(())
    }

    #[tokio::test]
    async fn coordinate_click_and_global_key_reach_the_page() -> Result<()> {
        // A full-viewport surface that records the last click's coordinates, plus
        // a focused input that records the last key it received. No selectors on
        // the click target — coordinates are the only way to hit it.
        let html = concat!(
            "<html><head><title>Canvas</title></head>",
            "<body style=\"margin:0\">",
            "<div id=\"pad\" style=\"position:absolute;left:0;top:0;width:300px;height:300px\" ",
            "onclick=\"document.getElementById('hit').innerText=Math.round(event.clientX)+','+Math.round(event.clientY)\"></div>",
            "<span id=\"hit\"></span>",
            "<input id=\"field\" style=\"position:absolute;left:0;top:320px\" ",
            "onkeydown=\"document.getElementById('key').innerText=event.key\">",
            "<span id=\"key\" style=\"position:absolute;left:0;top:360px\"></span>",
            "</body></html>"
        );
        let url = format!(
            "data:text/html;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(html)
        );

        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        {
            let mut context = fixture.context();
            let mut nav = BrowserNavigateInput { url, profile: None };
            BrowserNavigateTool.execute(&mut context, &mut nav).await?;

            // Click at exact CSS pixels inside the pad; the handler records them.
            let mut act = BrowserActInput {
                actions: vec![BrowserAction::ClickAt {
                    x: "120px".into(),
                    y: "80px".into(),
                }],
                profile: None,
            };
            let out = BrowserActTool.execute(&mut context, &mut act).await?;
            assert!(out.error.is_none(), "click_at error: {:?}", out.error);
        }

        let session = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap();
        let hit = session
            .eval("document.getElementById('hit').innerText")
            .await?;
        assert_eq!(
            hit.as_str().unwrap_or_default(),
            "120,80",
            "px click should land at the given CSS-pixel coordinates"
        );

        // Focus the field, then a global key press (no selector) lands on the
        // focused element.
        session
            .eval("document.getElementById('field').focus()")
            .await?;
        {
            let mut context = fixture.context();
            let mut act = BrowserActInput {
                actions: vec![BrowserAction::Press {
                    selector: None,
                    key: "a".into(),
                }],
                profile: None,
            };
            let out = BrowserActTool.execute(&mut context, &mut act).await?;
            assert!(out.error.is_none(), "global press error: {:?}", out.error);
        }
        let key = session
            .eval("document.getElementById('key').innerText")
            .await?;
        assert_eq!(
            key.as_str().unwrap_or_default(),
            "a",
            "selector-less press should reach the focused element"
        );
        Ok(())
    }

    #[test]
    fn resolve_coord_maps_units_and_rejects_typographic() {
        // Viewport 1000 × 500 CSS px.
        let (vw, vh) = (1000.0, 500.0);
        // vw/vh are always width/height.
        assert_eq!(resolve_coord("40vw", Axis::X, vw, vh).unwrap(), 400.0);
        assert_eq!(resolve_coord("30vh", Axis::Y, vw, vh).unwrap(), 150.0);
        // % follows the axis.
        assert_eq!(resolve_coord("50%", Axis::X, vw, vh).unwrap(), 500.0);
        assert_eq!(resolve_coord("50%", Axis::Y, vw, vh).unwrap(), 250.0);
        // px passes through unchanged.
        assert_eq!(resolve_coord("640px", Axis::X, vw, vh).unwrap(), 640.0);
        // Out-of-range clamps to the viewport.
        assert_eq!(resolve_coord("150vw", Axis::X, vw, vh).unwrap(), 1000.0);
        assert_eq!(resolve_coord("-10px", Axis::Y, vw, vh).unwrap(), 0.0);
        // Typographic units and bare numbers are rejected.
        assert!(resolve_coord("25rem", Axis::X, vw, vh).is_err());
        assert!(resolve_coord("2em", Axis::Y, vw, vh).is_err());
        assert!(resolve_coord("640", Axis::X, vw, vh).is_err());
    }

    #[tokio::test]
    async fn coordinate_units_map_to_the_same_css_pixel() -> Result<()> {
        // A full-viewport pad that records the CSS-pixel coords of the last click.
        let html = concat!(
            "<html><head><title>Pad</title></head><body style=\"margin:0\">",
            "<div id=\"pad\" style=\"position:fixed;inset:0\" ",
            "onclick=\"document.getElementById('hit').innerText=Math.round(event.clientX)+','+Math.round(event.clientY)\"></div>",
            "<span id=\"hit\" style=\"position:fixed;right:0;bottom:0\"></span>",
            "</body></html>"
        );
        let url = format!(
            "data:text/html;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(html)
        );

        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        {
            let mut context = fixture.context();
            let mut nav = BrowserNavigateInput { url, profile: None };
            BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        }

        let session = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap();
        let (vw, vh) = session.viewport_size().await?;
        assert!(vw > 0.0 && vh > 0.0, "viewport size should be known");

        // The centre of the viewport, expressed three ways, must land on the
        // same CSS pixel (±1 for rounding).
        let (cx, cy) = ((vw / 2.0).round(), (vh / 2.0).round());
        for (xu, yu) in [
            ("50%".to_string(), "50%".to_string()),
            ("50vw".to_string(), "50vh".to_string()),
            (format!("{cx}px"), format!("{cy}px")),
        ] {
            session
                .eval("document.getElementById('hit').innerText=''")
                .await?;
            let px = resolve_coord(&xu, Axis::X, vw, vh)?;
            let py = resolve_coord(&yu, Axis::Y, vw, vh)?;
            session.click_at(px, py).await?;
            let hit = session
                .eval("document.getElementById('hit').innerText")
                .await?;
            let got = hit.as_str().unwrap_or_default().to_string();
            let parts: Vec<f64> = got.split(',').filter_map(|s| s.parse().ok()).collect();
            assert_eq!(parts.len(), 2, "click {xu},{yu} recorded '{got}'");
            assert!(
                (parts[0] - cx).abs() <= 1.0 && (parts[1] - cy).abs() <= 1.0,
                "unit {xu}/{yu} should land at centre {cx},{cy}, got {got}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn acting_without_an_open_browser_is_a_clear_error() -> Result<()> {
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut read = BrowserReadInput {
            profile: None,
            full_page: false,
        };
        let out = BrowserReadTool.execute(&mut context, &mut read).await?;
        assert!(out.error.unwrap().contains("browser_navigate"));
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
            &demo_page_url(),
            false,
        )
        .await?;
        assert!(out.error.is_none(), "grant error: {:?}", out.error);
        assert!(out.observation.is_some(), "authenticated page observed");
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
            &demo_page_url(),
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
            url: demo_page_url(),
            profile: "elster".into(),
        };
        let out = BrowserLoginTool.execute(&mut context, &mut input).await?;
        assert!(out.error.unwrap().contains("interactive frontend"));
        Ok(())
    }

    #[test]
    fn error_output_is_text_only_even_when_a_screenshot_was_captured() {
        // browser_act's failure path captures a screenshot for context, then
        // sets an error. Anthropic rejects images in a tool_result with
        // is_error=true, so render_images must be empty on error.
        let out = BrowserOutput {
            profile: "default".into(),
            observation: None,
            screenshot_base64: Some("ZmFrZQ==".into()),
            error: Some("Action 1 failed: no such element '#missing'".into()),
        };
        assert!(!out.is_success());
        assert!(
            out.render_images().is_empty(),
            "an error result must carry no images"
        );
    }
}

#[cfg(test)]
mod registration_check {
    use crate::tools::scope::ToolScope;

    #[test]
    fn browser_tools_are_exposed_to_the_agent() {
        let registry = crate::tools::test_registry();
        let names: Vec<String> = registry
            .get_tool_definitions_with_capability(ToolScope::Agent.tag())
            .into_iter()
            .map(|d| d.name)
            .collect();
        for expected in [
            "browser_navigate",
            "browser_read",
            "browser_act",
            "browser_close",
            "browser_login",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected}; have: {names:?}"
            );
        }
    }
}
