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

async fn run_step(manager: &BrowserSessionManager, step: &BatchStep) -> Result<BrowserOutput> {
    fn parse<T: DeserializeOwned>(step: &BatchStep) -> Result<T> {
        serde_json::from_value(step.input.clone())
            .map_err(|e| anyhow::anyhow!("invalid input for {}: {e}", step.name))
    }
    let name = step.name.trim_start_matches("browser_");
    Ok(match name {
        "navigate" => page::navigate(manager, &parse(step)?).await,
        "read_page" => page::read_page(manager, &parse(step)?).await,
        "find" => page::find(manager, &parse(step)?).await,
        "get_page_text" => page::get_page_text(manager, &parse(step)?).await,
        "form_input" => page::form_input(manager, &parse(step)?).await,
        "javascript" => page::javascript(manager, &parse(step)?).await,
        "computer" => computer::computer(manager, &parse(step)?).await,
        "read_console_messages" => devtools::read_console(manager, &parse(step)?).await,
        "read_network_requests" => devtools::read_network(manager, &parse(step)?).await,
        "resize_window" => tabs::resize_window(manager, &parse(step)?).await,
        "tabs_context" => tabs::tabs_context(manager, &parse(step)?).await,
        "tabs_create" => tabs::tabs_create(manager, &parse(step)?).await,
        "tabs_select" => tabs::tabs_select(manager, &parse(step)?).await,
        "tabs_close" => tabs::tabs_close(manager, &parse(step)?).await,
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

        let mut bad = BatchInput {
            actions: vec![step("browser_login", json!({}))],
        };
        let out = BrowserBatchTool.execute(&mut context, &mut bad).await?;
        assert!(out.error.unwrap().contains("cannot run in a batch"));
        Ok(())
    }
}
