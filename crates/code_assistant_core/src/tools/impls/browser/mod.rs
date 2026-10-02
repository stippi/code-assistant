//! Browser tools, shaped after the computer-use style browser tools models
//! are trained on: a page is read as an accessibility tree whose elements
//! carry `ref_N` handles, acted on by ref or by screenshot coordinates, and
//! observed only when the model asks (screenshot, tree, text, console,
//! network) instead of after every step.
//!
//! State lives in the session-scoped `web::BrowserSessionManager` (see
//! [`crate::tools::services`]): one live browser per **profile**, each with
//! tabs. The `default` profile is a throwaway browser; a named profile
//! persists its login (see `browser_login` / `browser_profiles`).
//!
//! - [`page`] — `browser_navigate`, `browser_read_page`, `browser_find`,
//!   `browser_get_page_text`, `browser_form_input`, `browser_javascript`
//! - [`computer`] — `browser_computer`: mouse, keyboard, screenshot, zoom
//! - [`devtools`] — `browser_read_console_messages`,
//!   `browser_read_network_requests`
//! - [`tabs`] — `browser_tabs_*`, `browser_resize_window`
//! - [`batch`] — `browser_batch`: several steps in one call
//! - [`profiles`] — `browser_close`, `browser_login`, `browser_profiles`
//! - [`describe`] — one-line descriptions of calls, for the frontends

mod batch;
mod computer;
pub mod describe;
mod devtools;
mod page;
mod profiles;
mod tabs;

pub use batch::BrowserBatchTool;
pub use computer::BrowserComputerTool;
pub use devtools::{BrowserReadConsoleTool, BrowserReadNetworkTool};
pub use page::{
    BrowserFindTool, BrowserFormInputTool, BrowserGetPageTextTool, BrowserJavascriptTool,
    BrowserNavigateTool, BrowserReadPageTool,
};
pub use profiles::{BrowserCloseTool, BrowserLoginTool, BrowserProfilesTool};
pub use tabs::{
    BrowserResizeWindowTool, BrowserTabsCloseTool, BrowserTabsContextTool, BrowserTabsCreateTool,
    BrowserTabsSelectTool,
};

use crate::tools::core::{
    ImageData, Render, ResourcesTracker, ToolResult, ToolSpec, cap_base64_image, capabilities,
};
use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use web::{BrowserLaunchConfig, BrowserProfile, BrowserSession, BrowserSessionManager, Tab};

/// Register every browser tool.
pub fn register(registry: &mut crate::tools::core::ToolRegistry) {
    registry.register(Box::new(BrowserNavigateTool));
    registry.register(Box::new(BrowserComputerTool));
    registry.register(Box::new(BrowserReadPageTool));
    registry.register(Box::new(BrowserFindTool));
    registry.register(Box::new(BrowserGetPageTextTool));
    registry.register(Box::new(BrowserFormInputTool));
    registry.register(Box::new(BrowserJavascriptTool));
    registry.register(Box::new(BrowserReadConsoleTool));
    registry.register(Box::new(BrowserReadNetworkTool));
    registry.register(Box::new(BrowserResizeWindowTool));
    registry.register(Box::new(BrowserTabsContextTool));
    registry.register(Box::new(BrowserTabsCreateTool));
    registry.register(Box::new(BrowserTabsSelectTool));
    registry.register(Box::new(BrowserTabsCloseTool));
    registry.register(Box::new(BrowserBatchTool));
    registry.register(Box::new(BrowserCloseTool));
    registry.register(Box::new(BrowserLoginTool));
    registry.register(Box::new(BrowserProfilesTool));
}

/// The profile used when the model does not name one: a reusable ephemeral
/// (throwaway) browser.
pub(crate) const DEFAULT_PROFILE: &str = "default";

/// Resolve a profile name to a launch config. The reserved `"default"` name is
/// an ephemeral throwaway browser; any other name is a persistent profile under
/// `<config_dir>/browser-profiles/<name>`, so a login can be reused across runs.
pub(crate) fn launch_config_for(profile: &str, headful: bool) -> BrowserLaunchConfig {
    if profile == DEFAULT_PROFILE {
        return BrowserLaunchConfig {
            profile: BrowserProfile::Ephemeral,
            headful,
        };
    }
    let dir = profiles::profiles_root().join(profiles::sanitize_profile(profile));
    BrowserLaunchConfig {
        profile: BrowserProfile::Persistent(dir),
        headful,
    }
}

/// Get the live browser for `profile`, opening one if none exists yet.
pub(crate) async fn get_or_open(
    manager: &BrowserSessionManager,
    profile: &str,
) -> Result<Arc<BrowserSession>> {
    if let Some(session) = manager.get_by_label(profile) {
        return Ok(session);
    }
    let session = Arc::new(BrowserSession::open(launch_config_for(profile, false), profile).await?);
    manager.register(session.clone(), profile);
    Ok(session)
}

/// Which browser and tab a call targets. Flattened into every tool's input.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Target {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

impl Target {
    pub(crate) fn profile(&self) -> &str {
        self.profile.as_deref().unwrap_or(DEFAULT_PROFILE)
    }
}

/// The schema properties every tool shares.
pub(crate) fn target_properties(schema: &mut Value) {
    let props = schema["properties"].as_object_mut().expect("object schema");
    props.insert(
        "profile".into(),
        serde_json::json!({"type": "string", "description": "Named persistent profile (see browser_profiles); omit for the throwaway browser"}),
    );
    props.insert(
        "tab_id".into(),
        serde_json::json!({"type": "string", "description": "Tab to act on; omit for the active tab"}),
    );
}

/// A resolved browser + tab, plus what changed around the call.
pub(crate) struct Resolved {
    pub profile: String,
    pub session: Arc<BrowserSession>,
    pub tab: Arc<Tab>,
    /// Tabs the page opened since the last call.
    adopted: Vec<String>,
    url_before: String,
}

impl Resolved {
    /// Resolve `target`, opening the profile's browser if `open` and none is
    /// running. A failure comes back as the output to return.
    pub async fn new(
        manager: &BrowserSessionManager,
        target: &Target,
        open: bool,
    ) -> std::result::Result<Self, BrowserOutput> {
        let profile = target.profile().to_string();
        let session = if open {
            get_or_open(manager, &profile).await.map_err(|e| {
                BrowserOutput::failure(&profile, format!("Failed to open browser: {e}"))
            })?
        } else {
            manager.get_by_label(&profile).ok_or_else(|| {
                BrowserOutput::failure(
                    &profile,
                    format!(
                        "No browser is open for profile '{profile}'. Use browser_navigate first."
                    ),
                )
            })?
        };
        let adopted = session.sync_tabs().await.unwrap_or_default();
        let tab = session
            .tab(target.tab_id.as_deref())
            .map_err(|e| BrowserOutput::failure(&profile, e.to_string()))?;
        let url_before = tab.location().await.0;
        Ok(Self {
            profile,
            session,
            tab,
            adopted,
            url_before,
        })
    }

    /// Things the model should know after an action: dialogs answered on its
    /// behalf, tabs the page opened, and a navigation the action caused.
    pub async fn notes(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        for dialog in self.tab.take_dialogs() {
            let verdict = if dialog.accepted {
                "accepted"
            } else {
                "dismissed"
            };
            notes.push(format!(
                "A {} dialog \"{}\" was {verdict}.",
                dialog.kind, dialog.message
            ));
        }
        self.adopted
            .extend(self.session.sync_tabs().await.unwrap_or_default());
        for id in self.adopted.drain(..) {
            if let Ok(tab) = self.session.tab(Some(&id)) {
                let (url, title) = tab.location().await;
                notes.push(format!(
                    "The page opened a new tab {id}: {url} — {title} (switch with browser_tabs_select)."
                ));
            }
        }
        let (url, title) = self.tab.location().await;
        if url != self.url_before {
            notes.push(format!("The page navigated to {url} — {title}"));
            self.url_before = url;
        }
        notes
    }

    pub fn output(&self, text: impl Into<String>) -> BrowserOutput {
        BrowserOutput {
            profile: self.profile.clone(),
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn failure(&self, error: impl std::fmt::Display) -> BrowserOutput {
        BrowserOutput::failure(&self.profile, error.to_string())
    }
}

/// Text plus optional screenshots, the result of every browser tool.
///
/// Every field defaults, so outputs stored by older versions (with other
/// fields) still load.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BrowserOutput {
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub text: String,
    /// Base64 PNGs, surfaced to the model via `render_images`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BrowserOutput {
    pub(crate) fn failure(profile: &str, error: impl Into<String>) -> Self {
        Self {
            profile: profile.to_string(),
            error: Some(error.into()),
            ..Default::default()
        }
    }

    /// The output when this context has no browser registry.
    pub(crate) fn unavailable(profile: &str) -> Self {
        Self::failure(
            profile,
            "Browser tools are not available in this context (no browser session registry).",
        )
    }

    pub(crate) fn with_image(mut self, png: &[u8]) -> Self {
        self.images
            .push(base64::engine::general_purpose::STANDARD.encode(png));
        self
    }

    /// Append notes (dialogs, new tabs, navigation) after the main text.
    pub(crate) fn with_notes(mut self, notes: Vec<String>) -> Self {
        for note in notes {
            if !self.text.is_empty() {
                self.text.push('\n');
            }
            self.text.push_str(&format!("Note: {note}"));
        }
        self
    }
}

impl Render for BrowserOutput {
    fn status(&self) -> String {
        match &self.error {
            Some(e) => format!("Browser error: {e}"),
            None => self.text.lines().next().unwrap_or("Done").to_string(),
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        match &self.error {
            Some(e) if self.text.is_empty() => format!("Browser error: {e}"),
            Some(e) => format!("{}\nBrowser error: {e}", self.text),
            None if self.text.is_empty() => "Done.".to_string(),
            None => self.text.clone(),
        }
    }

    fn render_images(&self) -> Vec<ImageData> {
        // An error tool result must be text-only (Anthropic rejects images in a
        // tool_result with is_error=true).
        if self.error.is_some() {
            return Vec::new();
        }
        self.images
            .iter()
            .map(|data| ImageData {
                media_type: "image/png".to_string(),
                base64_data: data.clone(),
            })
            .collect()
    }

    fn cap_images(&mut self, max_edge: u32) {
        for data in &mut self.images {
            if let Some((_, capped)) = cap_base64_image("image/png", data, max_edge) {
                *data = capped;
            }
        }
    }
}

impl ToolResult for BrowserOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Build a browser tool's spec. `read_only` tools are also offered to
/// read-only sub-agents.
pub(crate) fn spec(
    name: &'static str,
    description: &'static str,
    mut parameters_schema: Value,
    read_only: bool,
    title_template: &'static str,
) -> ToolSpec {
    target_properties(&mut parameters_schema);
    let mut caps = vec![
        capabilities::SCOPE_AGENT,
        capabilities::SCOPE_AGENT_DIFF,
        capabilities::SCOPE_SUBAGENT_DEFAULT,
        capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
    ];
    if read_only {
        caps.push(capabilities::READ_ONLY);
        caps.push(capabilities::SCOPE_SUBAGENT_READ_ONLY);
    }
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters_schema,
        annotations: Some(serde_json::json!({"readOnlyHint": read_only, "openWorldHint": true})),
        capabilities: ToolSpec::capabilities(&caps),
        multiline_params: &[],
        hidden: false,
        title_template: Some(title_template),
    }
}

/// Implement `Tool` for a browser tool whose work is `run(manager, input)`.
macro_rules! browser_tool {
    ($tool:ident, $input:ty, $run:path) => {
        #[async_trait::async_trait]
        impl crate::tools::core::Tool for $tool {
            type Input = $input;
            type Output = $crate::tools::impls::browser::BrowserOutput;

            fn spec(&self) -> crate::tools::core::ToolSpec {
                Self::make_spec()
            }

            async fn execute<'a>(
                &self,
                context: &mut crate::tools::core::ToolContext<'a>,
                input: &mut Self::Input,
            ) -> anyhow::Result<Self::Output> {
                use crate::tools::services::ToolServicesAccess;
                let Some(manager) = context.browser_sessions() else {
                    return Ok($crate::tools::impls::browser::BrowserOutput::unavailable(
                        input.target.profile(),
                    ));
                };
                Ok($run(manager, input).await)
            }
        }
    };
}
pub(crate) use browser_tool;

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use std::time::Duration;

    pub fn data_url(html: &str) -> String {
        format!(
            "data:text/html;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(html)
        )
    }

    /// The first `ref_N` in `text` on a line containing `needle`.
    pub fn ref_on_line(text: &str, needle: &str) -> String {
        let line = text
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{text}"));
        line.split(['[', ']'])
            .find(|p| p.starts_with("ref_"))
            .unwrap_or_else(|| panic!("no ref on {line:?}"))
            .to_string()
    }

    /// Register a throwaway browser with short limits as the default profile,
    /// so tools pick it up instead of launching one with the real limits.
    pub async fn register_short_timeout_browser(fixture: &ToolTestFixture) -> Result<()> {
        let session = BrowserSession::open(BrowserLaunchConfig::default(), DEFAULT_PROFILE)
            .await?
            .with_timeouts(web::BrowserTimeouts {
                command: Duration::from_secs(1),
                navigation: Duration::from_secs(2),
            });
        fixture
            .browser_sessions()
            .unwrap()
            .register(Arc::new(session), DEFAULT_PROFILE);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_output_is_text_only() {
        // Anthropic rejects images in a tool_result with is_error=true.
        let out = BrowserOutput {
            profile: "default".into(),
            text: "Screenshot".into(),
            images: vec!["ZmFrZQ==".into()],
            error: Some("step 2 failed".into()),
        };
        assert!(!out.is_success());
        assert!(out.render_images().is_empty());
        assert_eq!(
            out.render(&mut ResourcesTracker::default()),
            "Screenshot\nBrowser error: step 2 failed"
        );
    }

    #[test]
    fn outputs_of_older_versions_still_load() {
        let old = serde_json::json!({
            "profile": "default",
            "observation": {"url": "https://x.test", "title": "X", "text": "hi"},
            "screenshot_base64": "ZmFrZQ=="
        });
        let out: BrowserOutput = serde_json::from_value(old).unwrap();
        assert!(out.is_success());
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

    #[test]
    fn browser_tools_are_exposed_to_the_agent() {
        use crate::tools::scope::ToolScope;
        let registry = crate::tools::test_registry();
        let names: Vec<String> = registry
            .get_tool_definitions_with_capability(ToolScope::Agent.tag())
            .into_iter()
            .map(|d| d.name)
            .collect();
        for expected in [
            "browser_navigate",
            "browser_computer",
            "browser_read_page",
            "browser_find",
            "browser_get_page_text",
            "browser_form_input",
            "browser_javascript",
            "browser_read_console_messages",
            "browser_read_network_requests",
            "browser_resize_window",
            "browser_tabs_context",
            "browser_tabs_create",
            "browser_tabs_select",
            "browser_tabs_close",
            "browser_batch",
            "browser_close",
            "browser_login",
            "browser_profiles",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected}; have: {names:?}"
            );
        }
    }
}
