//! `browser_batch`: several browser steps in one call.

use super::{BrowserOutput, DEFAULT_PROFILE, computer, devtools, page, spec, tabs};
use crate::tools::core::{Tool, ToolContext, ToolSpec};
use crate::tools::services::ToolServicesAccess;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web::BrowserSessionManager;

const MAX_STEPS: usize = 50;

#[derive(Deserialize, Serialize, Clone)]
pub struct BatchStep {
    pub name: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Deserialize, Serialize)]
pub struct BatchInput {
    pub actions: Vec<BatchStep>,
}

pub struct BrowserBatchTool;

#[async_trait::async_trait]
impl Tool for BrowserBatchTool {
    type Input = BatchInput;
    type Output = BrowserOutput;

    fn spec(&self) -> ToolSpec {
        let mut spec = spec(
            "browser_batch",
            "Run several browser tool calls in one round trip, in order, stopping at the first \
             error. Each step is {name, input} with the input that tool takes, e.g. \
             [{\"name\": \"browser_computer\", \"input\": {\"action\": \"left_click\", \"ref\": \
             \"ref_4\"}}, {\"name\": \"browser_computer\", \"input\": {\"action\": \"type\", \
             \"text\": \"hi\"}}, {\"name\": \"browser_computer\", \"input\": {\"action\": \
             \"screenshot\"}}]. Screenshots come back in order. Use it whenever you can predict \
             two or more steps; coordinates in a step refer to the screenshot taken before this \
             call. Steps run back to back, so this is also how to time input precisely: the page \
             keeps running between separate calls.",
            json!({
                "type": "object",
                "properties": {
                    "actions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "description": "A browser tool, e.g. browser_computer, browser_navigate, browser_form_input"},
                                "input": {"type": "object", "description": "That tool's input"}
                            },
                            "required": ["name", "input"]
                        }
                    }
                },
                "required": ["actions"]
            }),
            false,
            "Running browser steps",
        );
        // Each step names its own profile and tab.
        if let Some(props) = spec.parameters_schema["properties"].as_object_mut() {
            props.remove("profile");
            props.remove("tab_id");
        }
        spec
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let Some(manager) = context.browser_sessions() else {
            return Ok(BrowserOutput::unavailable(DEFAULT_PROFILE));
        };
        Ok(run_batch(manager, &input.actions).await)
    }
}

pub(crate) async fn run_batch(
    manager: &BrowserSessionManager,
    steps: &[BatchStep],
) -> BrowserOutput {
    if steps.len() > MAX_STEPS {
        return BrowserOutput::failure(
            DEFAULT_PROFILE,
            format!("at most {MAX_STEPS} steps per batch"),
        );
    }
    let mut combined = BrowserOutput {
        profile: DEFAULT_PROFILE.to_string(),
        ..Default::default()
    };
    let mut sections = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let out = match run_step(manager, step).await {
            Ok(out) => out,
            Err(e) => BrowserOutput::failure(DEFAULT_PROFILE, e.to_string()),
        };
        let label = step_label(step);
        if !out.text.is_empty() {
            sections.push(format!("[{n}] {label}\n{}", out.text));
        }
        combined.profile = out.profile;
        if let Some(e) = out.error {
            combined.error = Some(format!("Step {n} ({label}) failed: {e}"));
            break;
        }
        combined.images.extend(out.images);
    }
    combined.text = sections.join("\n\n");
    combined
}

fn step_label(step: &BatchStep) -> String {
    let name = step.name.trim_start_matches("browser_");
    match step.input.get("action").and_then(Value::as_str) {
        Some(action) => format!("{name} {action}"),
        None => name.to_string(),
    }
}

/// Parse a step's input the way a direct call of `tool` would: coerced
/// against that tool's schema first, so empty placeholders and lone scalars
/// behave the same inside a batch.
fn parse_step<T: Tool>(step: &BatchStep, tool: T) -> Result<T::Input>
where
    T::Input: DeserializeOwned,
{
    let mut input = step.input.clone();
    tools_core::coerce::coerce_to_schema(&mut input, &tool.spec().parameters_schema);
    serde_json::from_value(input)
        .map_err(|e| anyhow::anyhow!("invalid input for {}: {e}", step.name))
}

async fn run_step(manager: &BrowserSessionManager, step: &BatchStep) -> Result<BrowserOutput> {
    let name = step.name.trim_start_matches("browser_");
    Ok(match name {
        "navigate" => page::navigate(manager, &parse_step(step, page::BrowserNavigateTool)?).await,
        "read_page" => {
            page::read_page(manager, &parse_step(step, page::BrowserReadPageTool)?).await
        }
        "find" => page::find(manager, &parse_step(step, page::BrowserFindTool)?).await,
        "get_page_text" => {
            page::get_page_text(manager, &parse_step(step, page::BrowserGetPageTextTool)?).await
        }
        "form_input" => {
            page::form_input(manager, &parse_step(step, page::BrowserFormInputTool)?).await
        }
        "javascript" => {
            page::javascript(manager, &parse_step(step, page::BrowserJavascriptTool)?).await
        }
        "computer" => {
            computer::computer(manager, &parse_step(step, computer::BrowserComputerTool)?).await
        }
        "read_console_messages" => {
            devtools::read_console(
                manager,
                &parse_step(step, devtools::BrowserReadConsoleTool)?,
            )
            .await
        }
        "read_network_requests" => {
            devtools::read_network(
                manager,
                &parse_step(step, devtools::BrowserReadNetworkTool)?,
            )
            .await
        }
        "resize_window" => {
            tabs::resize_window(manager, &parse_step(step, tabs::BrowserResizeWindowTool)?).await
        }
        "tabs_context" => {
            tabs::tabs_context(manager, &parse_step(step, tabs::BrowserTabsContextTool)?).await
        }
        "tabs_create" => {
            tabs::tabs_create(manager, &parse_step(step, tabs::BrowserTabsCreateTool)?).await
        }
        "tabs_select" => {
            tabs::tabs_select(manager, &parse_step(step, tabs::BrowserTabsSelectTool)?).await
        }
        "tabs_close" => {
            tabs::tabs_close(manager, &parse_step(step, tabs::BrowserTabsCloseTool)?).await
        }
        other => anyhow::bail!("browser_{other} cannot run in a batch"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::data_url;
    use super::*;
    use crate::mocks::ToolTestFixture;
    use crate::tools::core::{Render, ResourcesTracker};

    fn step(name: &str, input: Value) -> BatchStep {
        BatchStep {
            name: name.into(),
            input,
        }
    }

    #[test]
    fn a_step_drops_empty_placeholders_like_a_direct_call() -> Result<()> {
        let input = parse_step(
            &step(
                "browser_computer",
                json!({"action": "left_click", "ref": "ref_7", "coordinate": [], "tab_id": ""}),
            ),
            computer::BrowserComputerTool,
        )?;
        assert_eq!(input.r#ref.as_deref(), Some("ref_7"));
        assert!(input.coordinate.is_none());
        assert!(input.target.tab_id.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn steps_run_in_order_and_stop_at_the_first_error() -> Result<()> {
        let page = data_url(
            "<html><body><input aria-label=\"Name\"><button onclick=\"document.title='done'\">Go</button></body></html>",
        );
        let mut fixture = ToolTestFixture::new().with_browser_sessions();
        let mut context = fixture.context();
        let mut input = BatchInput {
            actions: vec![
                step("browser_navigate", json!({"url": page})),
                step("browser_find", json!({"query": "button"})),
                step("computer", json!({"action": "screenshot", "scale": 0.25})),
                step(
                    "browser_javascript",
                    json!({"text": "document.title = 'x'; 1 + 1"}),
                ),
                step(
                    "browser_computer",
                    json!({"action": "left_click", "ref": "ref_999"}),
                ),
                step("browser_javascript", json!({"text": "'never runs'"})),
            ],
        };
        let out = BrowserBatchTool.execute(&mut context, &mut input).await?;
        let text = out.text.clone();
        assert!(text.contains("[1] navigate\n[t1]"), "{text}");
        assert!(text.contains("[2] find\n- button \"Go\" [ref_"), "{text}");
        assert!(
            text.contains("[3] computer screenshot\nScreenshot of t1 — 320×200 px"),
            "{text}"
        );
        assert!(text.contains("[4] javascript\n2"), "{text}");
        assert!(!text.contains("never runs"), "{text}");
        assert_eq!(out.images.len(), 1);
        let error = out.error.as_deref().unwrap();
        assert!(
            error.starts_with("Step 5 (computer left_click) failed: unknown ref"),
            "{error}"
        );
        assert!(
            out.render(&mut ResourcesTracker::default())
                .contains("Browser error: Step 5")
        );

        // Input held across a recording: the page moves while it is filmed.
        let mut walk = BatchInput {
            actions: vec![
                step(
                    "browser_javascript",
                    json!({"text": "window.x = 0; let held = false, last = 0;\
                      document.addEventListener('keydown', () => held = true);\
                      document.addEventListener('keyup', () => held = false);\
                      const step = (t) => { if (held) window.x += t - last; last = t; \
                        requestAnimationFrame(step); };\
                      requestAnimationFrame(step); 0"}),
                ),
                step("computer", json!({"action": "key_down", "text": "w"})),
                step(
                    "computer",
                    json!({"action": "record", "duration": 0.4, "frames": 4, "scale": 0.2}),
                ),
                step("computer", json!({"action": "key_up", "text": "w"})),
                step("javascript", json!({"text": "window.x"})),
            ],
        };
        let out = BrowserBatchTool.execute(&mut context, &mut walk).await?;
        assert!(out.error.is_none(), "{:?}", out.error);
        let text = out.text.clone();
        assert!(
            text.contains("[3] computer record\n4 frames over 0.4s"),
            "{text}"
        );
        assert!(text.contains("Note: Still held down: w"), "{text}");
        assert_eq!(out.images.len(), 1);
        let walked: f64 = text.rsplit('\n').next().unwrap().parse()?;
        assert!(walked >= 350.0, "held for {walked} ms\n{text}");

        let mut bad = BatchInput {
            actions: vec![step("browser_login", json!({}))],
        };
        let out = BrowserBatchTool.execute(&mut context, &mut bad).await?;
        assert!(out.error.unwrap().contains("cannot run in a batch"));
        Ok(())
    }
}
