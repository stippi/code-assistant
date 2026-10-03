//! What a tab logged: console messages and network requests.

use super::{BrowserOutput, Resolved, Target, browser_tool, spec};
use crate::tools::core::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::BrowserSessionManager;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// How much of a response body to return.
const MAX_BODY_CHARS: usize = 20_000;

// ---------------------------------------------------------------------------
// browser_read_console_messages
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct ReadConsoleInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_errors: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserReadConsoleTool;

impl BrowserReadConsoleTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_read_console_messages",
            "Read the tab's console output (log, info, warning, error, debug), uncaught \
             exceptions and browser messages such as failed resource loads — most recent last.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Max entries (default 50, max 200)"},
                    "only_errors": {"type": "boolean", "description": "Only errors and exceptions"},
                    "pattern": {"type": "string", "description": "Substring filter on the message text"}
                }
            }),
            true,
            "Reading console messages",
        )
    }
}

browser_tool!(BrowserReadConsoleTool, ReadConsoleInput, read_console);

pub(crate) async fn read_console(
    manager: &BrowserSessionManager,
    input: &ReadConsoleInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let lines = r
        .tab
        .console_messages(input.only_errors, input.pattern.as_deref(), limit);
    if lines.is_empty() {
        return r.output("No console messages.");
    }
    r.output(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// browser_read_network_requests
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct ReadNetworkInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserReadNetworkTool;

impl BrowserReadNetworkTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_read_network_requests",
            "List the tab's network requests (`[id] METHOD status type url (mime)`, most recent \
             last), or fetch one response body by `request_id`.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "Max entries when listing (default 50, max 200)"},
                    "url_pattern": {"type": "string", "description": "Substring filter on the URL"},
                    "request_id": {"type": "string", "description": "Return this request's response body instead of listing"}
                }
            }),
            true,
            "Reading network requests",
        )
    }
}

browser_tool!(BrowserReadNetworkTool, ReadNetworkInput, read_network);

pub(crate) async fn read_network(
    manager: &BrowserSessionManager,
    input: &ReadNetworkInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    if let Some(id) = &input.request_id {
        return match r.tab.response_body(id, MAX_BODY_CHARS).await {
            Ok(body) => r.output(body),
            Err(e) => r.failure(e),
        };
    }
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let lines = r.tab.network_requests(input.url_pattern.as_deref(), limit);
    if lines.is_empty() {
        return r.output("No network requests.");
    }
    r.output(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::super::page::{BrowserNavigateTool, NavigateInput};
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::tools::core::{Render, ResourcesTracker, Tool};
    use anyhow::Result;

    #[tokio::test]
    async fn console_errors_and_a_response_body() -> Result<()> {
        use axum::response::Html;
        use axum::{Router, routing::get};
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    Html(
                        "<html><body><script>console.log('ready'); console.error('broken');\
                          fetch('/api').then((r) => r.text());</script></body></html>",
                    )
                }),
            )
            .route("/api", get(|| async { "pong" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        BrowserNavigateTool
            .execute(
                &mut context,
                &mut NavigateInput {
                    url: format!("http://{addr}/"),
                    target: Target::default(),
                },
            )
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let render = |out: BrowserOutput| out.render(&mut ResourcesTracker::default());

        let mut console = ReadConsoleInput {
            limit: None,
            only_errors: true,
            pattern: None,
            target: Target::default(),
        };
        let errors = render(
            BrowserReadConsoleTool
                .execute(&mut context, &mut console)
                .await?,
        );
        assert!(errors.starts_with("[error] broken"), "{errors}");
        assert!(!errors.contains("ready"), "{errors}");

        let mut network = ReadNetworkInput {
            limit: None,
            url_pattern: Some("/api".into()),
            request_id: None,
            target: Target::default(),
        };
        let listed = render(
            BrowserReadNetworkTool
                .execute(&mut context, &mut network)
                .await?,
        );
        assert!(listed.contains("GET 200 fetch"), "{listed}");
        let id = listed.trim_start_matches('[').split(']').next().unwrap();
        network.request_id = Some(id.to_string());
        let body = render(
            BrowserReadNetworkTool
                .execute(&mut context, &mut network)
                .await?,
        );
        assert_eq!(body, "pong");
        Ok(())
    }
}
