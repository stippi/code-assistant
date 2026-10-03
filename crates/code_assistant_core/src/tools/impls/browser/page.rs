//! Reading and changing a page: navigate, the accessibility tree, text search,
//! page text, form input and scripts.

use super::{BrowserOutput, Resolved, Target, browser_tool, spec};
use crate::tools::core::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::json;
use web::{BrowserSessionManager, BrowserTimeout};

/// Default and ceiling for text-producing reads, in characters.
const DEFAULT_MAX_CHARS: usize = 50_000;

// ---------------------------------------------------------------------------
// browser_navigate
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct NavigateInput {
    pub url: String,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserNavigateTool;

impl BrowserNavigateTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_navigate",
            "Navigate a browser tab to a URL, or go \"back\"/\"forward\" in its history. Opens the \
             browser if none is running. Returns the resulting URL and title; look at the page \
             with browser_read_page, browser_computer (screenshot) or browser_get_page_text.",
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "URL to load (scheme optional, defaults to https), or \"back\"/\"forward\""}
                },
                "required": ["url"]
            }),
            true,
            "Navigating to {url}",
        )
    }
}

browser_tool!(BrowserNavigateTool, NavigateInput, navigate);

pub(crate) async fn navigate(
    manager: &BrowserSessionManager,
    input: &NavigateInput,
) -> BrowserOutput {
    let mut r = match Resolved::new(manager, &input.target, true).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    let mut notes = Vec::new();
    let result = match input.url.trim() {
        "back" => r.tab.history(-1).await,
        "forward" => r.tab.history(1).await,
        url => {
            let url =
                if url.contains("://") || url.starts_with("about:") || url.starts_with("data:") {
                    url.to_string()
                } else {
                    format!("https://{url}")
                };
            match r.tab.navigate(&url).await {
                // A load event that never comes (a slow iframe or tracker) is
                // no failure: the page is usually usable.
                Err(e) if e.downcast_ref::<BrowserTimeout>().is_some() => {
                    notes.push(format!(
                        "The page did not finish loading ({e}); it is shown as far as it got."
                    ));
                    Ok(())
                }
                other => other,
            }
        }
    };
    if let Err(e) = result {
        return r.failure(format!("Navigation failed: {e}"));
    }
    r.tab.settle().await;
    let (url, title) = r.tab.location().await;
    // The navigation itself is the headline, not a note.
    notes.extend(
        r.notes()
            .await
            .into_iter()
            .filter(|n| !n.starts_with("The page navigated")),
    );
    r.output(format!("[{}] {url}\nTitle: {title}", r.tab.id()))
        .with_notes(notes)
}

// ---------------------------------------------------------------------------
// browser_read_page
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadFilter {
    #[default]
    All,
    Interactive,
}

#[derive(Deserialize, Serialize)]
pub struct ReadPageInput {
    #[serde(default)]
    pub filter: ReadFilter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserReadPageTool;

impl BrowserReadPageTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_read_page",
            "Read the page as a YAML-style accessibility tree: `- role \"name\" [ref_N] state`. \
             Use the ref_N handles with browser_computer, browser_form_input and \
             browser_find. Prefer this over screenshots for text and structure.",
            json!({
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "enum": ["all", "interactive"], "description": "'interactive' lists only clickable/typable elements; 'all' (default) the full tree"},
                    "ref_id": {"type": "string", "description": "Only the subtree under this ref_N"},
                    "depth": {"type": "integer", "description": "Maximum tree depth (default 15)"},
                    "max_chars": {"type": "integer", "description": "Maximum output characters (default 50000)"}
                }
            }),
            true,
            "Reading the page",
        )
    }
}

browser_tool!(BrowserReadPageTool, ReadPageInput, read_page);

pub(crate) async fn read_page(
    manager: &BrowserSessionManager,
    input: &ReadPageInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    let lines = match r
        .tab
        .read_page(
            input.filter == ReadFilter::Interactive,
            input.ref_id.as_deref(),
            input.depth.unwrap_or(15),
        )
        .await
    {
        Ok(lines) => lines,
        Err(e) => return r.failure(e),
    };
    let (url, title) = r.tab.location().await;
    let header = format!("[{}] {url} — {title}", r.tab.id());
    let max_chars = input.max_chars.unwrap_or(DEFAULT_MAX_CHARS);
    r.output(format!("{header}\n{}", join_bounded(&lines, max_chars)))
}

/// Join lines up to `max_chars`, cutting at a line boundary with a note.
fn join_bounded(lines: &[String], max_chars: usize) -> String {
    let mut out = String::new();
    for line in lines {
        if out.len() + line.len() + 1 > max_chars {
            out.push_str(&format!(
                "… (truncated at {max_chars} characters; narrow it with ref_id, depth or filter)"
            ));
            return out;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

// ---------------------------------------------------------------------------
// browser_find
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct FindInput {
    pub query: String,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserFindTool;

impl BrowserFindTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_find",
            "Search the page's accessibility tree for elements whose role, name or state \
             contains `query` (case-insensitive). Returns up to 20 matching lines with ref_N \
             handles.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Text to look for, e.g. \"Sign in\", \"search\", \"checkbox\""}
                },
                "required": ["query"]
            }),
            true,
            "Finding \"{query}\"",
        )
    }
}

browser_tool!(BrowserFindTool, FindInput, find);

pub(crate) async fn find(manager: &BrowserSessionManager, input: &FindInput) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    match r.tab.find(&input.query, 20).await {
        Ok(lines) if lines.is_empty() => {
            r.output(format!("No elements match \"{}\".", input.query))
        }
        Ok(lines) => r.output(lines.join("\n")),
        Err(e) => r.failure(e),
    }
}

// ---------------------------------------------------------------------------
// browser_get_page_text
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct GetPageTextInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserGetPageTextTool;

impl BrowserGetPageTextTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_get_page_text",
            "Extract the visible text of the page (its main/article content if it has one, \
             else the body).",
            json!({
                "type": "object",
                "properties": {
                    "max_chars": {"type": "integer", "description": "Maximum characters (default 50000)"}
                }
            }),
            true,
            "Reading the page text",
        )
    }
}

browser_tool!(BrowserGetPageTextTool, GetPageTextInput, get_page_text);

pub(crate) async fn get_page_text(
    manager: &BrowserSessionManager,
    input: &GetPageTextInput,
) -> BrowserOutput {
    let r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    match r
        .tab
        .page_text(input.max_chars.unwrap_or(DEFAULT_MAX_CHARS))
        .await
    {
        Ok(text) => {
            let (url, title) = r.tab.location().await;
            r.output(format!("[{}] {url} — {title}\n\n{text}", r.tab.id()))
        }
        Err(e) => r.failure(e),
    }
}

// ---------------------------------------------------------------------------
// browser_form_input
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct FormInputInput {
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub value: serde_json::Value,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserFormInputTool;

impl BrowserFormInputTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_form_input",
            "Set a form control by ref: a select by option value or visible text (an array for \
             multi-select), a checkbox/radio/switch by true/false, a text field or \
             contenteditable by text. Fires the input/change events pages listen for.",
            json!({
                "type": "object",
                "properties": {
                    "ref": {"type": "string", "description": "ref_N from browser_read_page or browser_find"},
                    "value": {"description": "The value: string, boolean, number, or an array of strings for a multi-select"}
                },
                "required": ["ref", "value"]
            }),
            false,
            "Setting {ref}",
        )
    }
}

browser_tool!(BrowserFormInputTool, FormInputInput, form_input);

pub(crate) async fn form_input(
    manager: &BrowserSessionManager,
    input: &FormInputInput,
) -> BrowserOutput {
    let mut r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    match r.tab.form_input(&input.r#ref, &input.value).await {
        Ok(done) => {
            let notes = r.notes().await;
            r.output(format!("{}: {done}", input.r#ref))
                .with_notes(notes)
        }
        Err(e) => r.failure(e),
    }
}

// ---------------------------------------------------------------------------
// browser_javascript
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct JavascriptInput {
    pub text: String,
    #[serde(flatten)]
    pub target: Target,
}

pub struct BrowserJavascriptTool;

impl BrowserJavascriptTool {
    fn make_spec() -> ToolSpec {
        spec(
            "browser_javascript",
            "Execute JavaScript in the page for debugging and inspection. REPL semantics: \
             top-level await works and the value of the last expression is returned (as JSON) — \
             write the expression, not `return`.",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The code to run"}
                },
                "required": ["text"]
            }),
            false,
            "Running JavaScript",
        )
    }
}

browser_tool!(BrowserJavascriptTool, JavascriptInput, javascript);

pub(crate) async fn javascript(
    manager: &BrowserSessionManager,
    input: &JavascriptInput,
) -> BrowserOutput {
    let mut r = match Resolved::new(manager, &input.target, false).await {
        Ok(r) => r,
        Err(out) => return out,
    };
    match r.tab.javascript(&input.text).await {
        Ok(value) => {
            let notes = r.notes().await;
            r.output(value).with_notes(notes)
        }
        Err(e) => r.failure(format!("JavaScript error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::tools::core::{Render, ResourcesTracker, Tool};
    use anyhow::Result;

    fn render(out: &BrowserOutput) -> String {
        out.render(&mut ResourcesTracker::default())
    }

    #[tokio::test]
    async fn navigate_read_find_fill_and_inspect() -> Result<()> {
        let page = data_url(
            "<html><head><title>Form</title></head><body><h1>Welcome</h1>\
             <label>Name <input id=\"n\"></label>\
             <label>Size <select id=\"s\"><option>Small</option><option>Large</option></select></label>\
             <main><p>Main text</p></main></body></html>",
        );
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();

        let mut nav = NavigateInput {
            url: page,
            target: Target::default(),
        };
        let out = BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(render(&out).contains("Title: Form"), "{}", render(&out));
        assert!(out.images.is_empty(), "navigate returns no screenshot");

        let mut read = ReadPageInput {
            filter: ReadFilter::Interactive,
            ref_id: None,
            depth: None,
            max_chars: None,
            target: Target::default(),
        };
        let tree = render(&BrowserReadPageTool.execute(&mut context, &mut read).await?);
        let name = ref_on_line(&tree, "textbox \"Name\"");
        let size = ref_on_line(&tree, "combobox \"Size\"");

        let mut find = FindInput {
            query: "welcome".into(),
            target: Target::default(),
        };
        let found = render(&BrowserFindTool.execute(&mut context, &mut find).await?);
        assert!(found.starts_with("- heading \"Welcome\""), "{found}");

        for (r, value, expect) in [
            (&name, json!("Ada"), "set value"),
            (&size, json!("Large"), "selected Large"),
        ] {
            let mut input = FormInputInput {
                r#ref: r.clone(),
                value,
                target: Target::default(),
            };
            let out = BrowserFormInputTool
                .execute(&mut context, &mut input)
                .await?;
            assert!(render(&out).contains(expect), "{}", render(&out));
        }

        let mut js = JavascriptInput {
            text: "[document.getElementById('n').value, document.getElementById('s').value]".into(),
            target: Target::default(),
        };
        let out = BrowserJavascriptTool.execute(&mut context, &mut js).await?;
        assert_eq!(render(&out), r#"["Ada","Large"]"#);

        let mut text = GetPageTextInput {
            max_chars: None,
            target: Target::default(),
        };
        let out = BrowserGetPageTextTool
            .execute(&mut context, &mut text)
            .await?;
        assert!(render(&out).ends_with("\n\nMain text"), "{}", render(&out));
        Ok(())
    }

    #[tokio::test]
    async fn reading_without_an_open_browser_is_a_clear_error() -> Result<()> {
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut read = ReadPageInput {
            filter: ReadFilter::All,
            ref_id: None,
            depth: None,
            max_chars: None,
            target: Target::default(),
        };
        let out = BrowserReadPageTool.execute(&mut context, &mut read).await?;
        assert!(out.error.unwrap().contains("browser_navigate"));
        Ok(())
    }

    #[tokio::test]
    async fn a_page_that_never_finishes_loading_is_shown_with_a_note() -> Result<()> {
        use axum::response::Html;
        use axum::{Router, routing::get};
        let app = Router::new()
            .route(
                "/slow",
                get(|| async {
                    Html(
                        "<html><head><title>Slow</title></head><body><h1>Main content</h1>\
                          <iframe src=\"/never\"></iframe></body></html>",
                    )
                }),
            )
            .route(
                "/never",
                get(|| async {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    Html("")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        register_short_timeout_browser(&fixture).await?;
        let mut context = fixture.context();
        let mut nav = NavigateInput {
            url: format!("http://{addr}/slow"),
            target: Target::default(),
        };
        let out = BrowserNavigateTool.execute(&mut context, &mut nav).await?;
        assert!(out.error.is_none(), "{:?}", out.error);
        let text = render(&out);
        assert!(text.contains("Title: Slow"), "{text}");
        assert!(text.contains("did not finish loading"), "{text}");
        Ok(())
    }

    #[test]
    fn join_bounded_cuts_at_a_line() {
        let lines = vec!["- a".to_string(), "- bbbb".to_string(), "- c".to_string()];
        assert_eq!(join_bounded(&lines, 100), "- a\n- bbbb\n- c");
        let cut = join_bounded(&lines, 8);
        assert!(cut.starts_with("- a\n…"), "{cut}");
    }
}
