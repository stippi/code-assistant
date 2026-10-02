//! Tabs of a profile's browser, and the viewport a tab emulates.

use super::{BrowserOutput, Resolved, Target, browser_tool, get_or_open, spec};
use crate::tools::core::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::BrowserSessionManager;

/// Drop the `tab_id` property for tools that are not about one tab.
fn without_tab_id(mut spec: ToolSpec) -> ToolSpec {
    if let Some(props) = spec.parameters_schema["properties"].as_object_mut() {
        props.remove("tab_id");
    }
    spec
}

/// Make `tab_id` required for tools about one specific tab.
fn requiring_tab_id(mut spec: ToolSpec) -> ToolSpec {
    spec.parameters_schema["properties"]["tab_id"] =
        json!({"type": "string", "description": "The tab, as listed by browser_tabs_context"});
    spec.parameters_schema["required"] = json!(["tab_id"]);
    spec
}

// ---------------------------------------------------------------------------
// browser_tabs_context
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct TabsContextInput {
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserTabsContextTool;

impl BrowserTabsContextTool {
    fn make_spec() -> ToolSpec {
        without_tab_id(spec(
            "browser_tabs_context",
            "List the browser's tabs with their id, URL and title; the active tab is the one \
             tools use when no tab_id is given.",
            json!({"type": "object", "properties": {}}),
            true,
            "Listing tabs",
        ))
    }
}

browser_tool!(BrowserTabsContextTool, TabsContextInput, tabs_context);

pub(crate) async fn tabs_context(
    manager: &BrowserSessionManager,
    input: &TabsContextInput,
) -> BrowserOutput {
    let profile = input.target.profile();
    let Some(session) = manager.get_by_label(profile) else {
        return BrowserOutput {
            profile: profile.to_string(),
            text: format!("No browser is open for profile '{profile}'."),
            ..Default::default()
        };
    };
    let _ = session.sync_tabs().await;
    let lines: Vec<String> = session
        .tabs()
        .await
        .into_iter()
        .map(|t| {
            let active = if t.active { " (active)" } else { "" };
            format!("{}{active}: {} — {}", t.id, t.url, t.title)
        })
        .collect();
    BrowserOutput {
        profile: profile.to_string(),
        text: lines.join("\n"),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// browser_tabs_create
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct TabsCreateInput {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub foreground: bool,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserTabsCreateTool;

impl BrowserTabsCreateTool {
    fn make_spec() -> ToolSpec {
        without_tab_id(spec(
            "browser_tabs_create",
            "Open a new blank tab (opening the browser if needed) and return its tab_id. It \
             opens in the background unless `foreground` is true; load a page with \
             browser_navigate and its tab_id.",
            json!({
                "type": "object",
                "properties": {
                    "foreground": {"type": "boolean", "description": "Make it the active tab"}
                }
            }),
            false,
            "Opening a tab",
        ))
    }
}

browser_tool!(BrowserTabsCreateTool, TabsCreateInput, tabs_create);

pub(crate) async fn tabs_create(
    manager: &BrowserSessionManager,
    input: &TabsCreateInput,
) -> BrowserOutput {
    let profile = input.target.profile();
    let was_open = manager.get_by_label(profile).is_some();
    let session = match get_or_open(manager, profile).await {
        Ok(s) => s,
        Err(e) => return BrowserOutput::failure(profile, format!("Failed to open browser: {e}")),
    };
    // A browser that was just opened already has its fresh blank tab.
    let tab = if was_open {
        session.create_tab(input.foreground).await
    } else {
        session.active_tab()
    };
    match tab {
        Ok(tab) => {
            let active = session
                .active_tab()
                .map(|t| t.id() == tab.id())
                .unwrap_or(false);
            BrowserOutput {
                profile: profile.to_string(),
                text: format!(
                    "Opened tab {}{}",
                    tab.id(),
                    if active {
                        " (active)"
                    } else {
                        " (in the background)"
                    }
                ),
                ..Default::default()
            }
        }
        Err(e) => BrowserOutput::failure(profile, e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// browser_tabs_select / browser_tabs_close
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct TabIdInput {
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserTabsSelectTool;

impl BrowserTabsSelectTool {
    fn make_spec() -> ToolSpec {
        requiring_tab_id(spec(
            "browser_tabs_select",
            "Make a tab the active one, which tools use when no tab_id is given.",
            json!({"type": "object", "properties": {}}),
            false,
            "Selecting tab {tab_id}",
        ))
    }
}

browser_tool!(BrowserTabsSelectTool, TabIdInput, tabs_select);

pub(crate) async fn tabs_select(
    manager: &BrowserSessionManager,
    input: &TabIdInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    if let Err(e) = r.session.select_tab(r.tab.id()) {
        return r.failure(e);
    }
    let (url, title) = r.tab.location().await;
    r.output(format!("Active tab {}: {url} — {title}", r.tab.id()))
}

pub struct BrowserTabsCloseTool;

impl BrowserTabsCloseTool {
    fn make_spec() -> ToolSpec {
        requiring_tab_id(spec(
            "browser_tabs_close",
            "Close a tab. Closing the last tab closes the browser.",
            json!({"type": "object", "properties": {}}),
            false,
            "Closing tab {tab_id}",
        ))
    }
}

browser_tool!(BrowserTabsCloseTool, TabIdInput, tabs_close);

pub(crate) async fn tabs_close(
    manager: &BrowserSessionManager,
    input: &TabIdInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    let id = r.tab.id().to_string();
    if r.session.tabs().await.len() <= 1 {
        if let Some(session) = manager.remove_by_label(&r.profile) {
            session.close().await;
        }
        return r.output(format!(
            "Closed {id}, the last tab, so the browser was closed."
        ));
    }
    match r.session.close_tab(&id).await {
        Ok(()) => {
            let active = r
                .session
                .active_tab()
                .map(|t| t.id().to_string())
                .unwrap_or_default();
            r.output(format!("Closed {id}. Active tab: {active}"))
        }
        Err(e) => r.failure(e),
    }
}

// ---------------------------------------------------------------------------
// browser_resize_window
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Mobile,
    Tablet,
    Desktop,
}

#[derive(Deserialize, Serialize)]
pub struct ResizeInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserResizeWindowTool;

impl BrowserResizeWindowTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_resize_window",
            "Emulate a viewport size for a tab: presets mobile (375×812), tablet (768×1024), \
             desktop (1280×800, the default), or width + height. Below 768 px wide it also \
             emulates a phone (touch, Android user agent) — reload the page afterwards. \
             `color_scheme` emulates prefers-color-scheme.",
            json!({
                "type": "object",
                "properties": {
                    "preset": {"type": "string", "enum": ["mobile", "tablet", "desktop"]},
                    "width": {"type": "integer"},
                    "height": {"type": "integer"},
                    "color_scheme": {"type": "string", "enum": ["light", "dark"]}
                }
            }),
            false,
            "Resizing the viewport",
        )
    }
}

browser_tool!(BrowserResizeWindowTool, ResizeInput, resize_window);

pub(crate) async fn resize_window(
    manager: &BrowserSessionManager,
    input: &ResizeInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    let size = match (input.preset, input.width, input.height) {
        (Some(Preset::Mobile), _, _) => Some((375, 812)),
        (Some(Preset::Tablet), _, _) => Some((768, 1024)),
        (Some(Preset::Desktop), _, _) => Some((web::DEFAULT_VIEWPORT.0, web::DEFAULT_VIEWPORT.1)),
        (None, Some(w), Some(h)) => Some((w, h)),
        (None, None, None) => None,
        _ => return r.failure("give a preset, or both width and height"),
    };
    let mut done = Vec::new();
    if let Some((w, h)) = size {
        let mobile = w < 768;
        if let Err(e) = r.tab.set_viewport(w, h, mobile).await {
            return r.failure(e);
        }
        done.push(if mobile {
            format!("Viewport {w}×{h} with phone emulation (reload the page to apply touch and user agent)")
        } else {
            format!("Viewport {w}×{h}")
        });
    }
    if let Some(scheme) = &input.color_scheme {
        if let Err(e) = r.tab.set_color_scheme(Some(scheme)).await {
            return r.failure(e);
        }
        done.push(format!("prefers-color-scheme: {scheme}"));
    }
    if done.is_empty() {
        return r.failure("nothing to change: give a preset, width + height, or color_scheme");
    }
    r.output(done.join("; "))
}

#[cfg(test)]
mod tests {
    use super::super::page::{BrowserNavigateTool, NavigateInput};
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::tools::core::{Render, ResourcesTracker, Tool};
    use anyhow::Result;

    fn render(out: BrowserOutput) -> String {
        out.render(&mut ResourcesTracker::default())
    }

    fn on_tab(tab_id: &str) -> TabIdInput {
        TabIdInput {
            target: Target {
                profile: None,
                tab_id: Some(tab_id.into()),
            },
        }
    }

    #[tokio::test]
    async fn tabs_open_list_select_and_close() -> Result<()> {
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut list = TabsContextInput {
            target: Target::default(),
        };
        assert!(
            render(
                BrowserTabsContextTool
                    .execute(&mut context, &mut list)
                    .await?
            )
            .contains("No browser")
        );

        let mut create = TabsCreateInput {
            foreground: false,
            target: Target::default(),
        };
        assert_eq!(
            render(
                BrowserTabsCreateTool
                    .execute(&mut context, &mut create)
                    .await?
            ),
            "Opened tab t1 (active)"
        );
        assert_eq!(
            render(
                BrowserTabsCreateTool
                    .execute(&mut context, &mut create)
                    .await?
            ),
            "Opened tab t2 (in the background)"
        );
        let mut nav = NavigateInput {
            url: "data:text/html,<title>Two</title>".into(),
            target: Target {
                profile: None,
                tab_id: Some("t2".into()),
            },
        };
        BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        let listed = render(
            BrowserTabsContextTool
                .execute(&mut context, &mut list)
                .await?,
        );
        assert!(listed.starts_with("t1 (active): about:blank"), "{listed}");
        assert!(
            listed.contains("t2: data:text/html,<title>Two</title> — Two"),
            "{listed}"
        );

        let out = render(
            BrowserTabsSelectTool
                .execute(&mut context, &mut on_tab("t2"))
                .await?,
        );
        assert!(out.starts_with("Active tab t2"), "{out}");
        assert_eq!(
            render(
                BrowserTabsCloseTool
                    .execute(&mut context, &mut on_tab("t2"))
                    .await?
            ),
            "Closed t2. Active tab: t1"
        );
        let out = render(
            BrowserTabsCloseTool
                .execute(&mut context, &mut on_tab("t1"))
                .await?,
        );
        assert!(
            out.contains("the last tab, so the browser was closed"),
            "{out}"
        );
        assert!(
            fixture
                .browser_sessions()
                .unwrap()
                .get_by_label("default")
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn resize_emulates_a_phone_and_dark_mode() -> Result<()> {
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut nav = NavigateInput {
            url: "data:text/html,<meta name=viewport content='width=device-width'>x".into(),
            target: Target::default(),
        };
        BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        let mut resize = ResizeInput {
            preset: Some(Preset::Mobile),
            width: None,
            height: None,
            color_scheme: Some("dark".into()),
            target: Target::default(),
        };
        let out = render(
            BrowserResizeWindowTool
                .execute(&mut context, &mut resize)
                .await?,
        );
        assert!(
            out.starts_with("Viewport 375×812 with phone emulation"),
            "{out}"
        );
        assert!(out.ends_with("prefers-color-scheme: dark"), "{out}");

        let tab = fixture
            .browser_sessions()
            .unwrap()
            .get_by_label("default")
            .unwrap()
            .active_tab()?;
        assert_eq!(
            tab.javascript("[innerWidth, matchMedia('(prefers-color-scheme: dark)').matches]")
                .await?,
            "[375,true]"
        );
        Ok(())
    }
}
