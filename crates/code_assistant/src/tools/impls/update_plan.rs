use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::types::{PlanItem, PlanItemPriority, PlanItemStatus, PlanState};
use crate::ui::UiEvent;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanEntryInput {
    pub content: String,
    #[serde(default)]
    pub priority: PlanItemPriority,
    #[serde(default)]
    pub status: PlanItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdatePlanInput {
    #[serde(default)]
    pub entries: Vec<PlanEntryInput>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlanOutput {
    pub summary: String,
    pub counts: PlanCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanCounts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl Render for UpdatePlanOutput {
    fn status(&self) -> String {
        self.summary.clone()
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        format!(
            "{} (pending: {}, in_progress: {}, completed: {})",
            self.summary, self.counts.pending, self.counts.in_progress, self.counts.completed
        )
    }
}

impl ToolResult for UpdatePlanOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct UpdatePlanTool;

impl UpdatePlanTool {
    fn spec_description() -> &'static str {
        "Replace the current execution plan with the provided list of items. Supply the full plan each time."
    }

    fn build_plan(entries: &[PlanEntryInput], meta: Option<JsonValue>) -> Result<PlanState> {
        let mut plan_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let content = entry.content.trim();
            if content.is_empty() {
                return Err(anyhow!(
                    "Plan entries must include non-empty content. Received an empty entry."
                ));
            }
            plan_entries.push(PlanItem {
                content: content.to_string(),
                priority: entry.priority.clone(),
                status: entry.status.clone(),
                meta: entry.meta.clone(),
            });
        }

        Ok(PlanState {
            entries: plan_entries,
            meta,
        })
    }

    fn compute_counts(plan: &PlanState) -> PlanCounts {
        let mut counts = PlanCounts::default();
        for entry in &plan.entries {
            match entry.status {
                PlanItemStatus::Pending => counts.pending += 1,
                PlanItemStatus::InProgress => counts.in_progress += 1,
                PlanItemStatus::Completed => counts.completed += 1,
            }
        }
        counts
    }
}

#[async_trait::async_trait]
impl Tool for UpdatePlanTool {
    type Input = UpdatePlanInput;
    type Output = UpdatePlanOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_plan",
            description: Self::spec_description(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "entries": {
                        "type": "array",
                        "description": "Full list of plan items in order. Omit to clear the plan.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "Description of the task."
                                },
                                "priority": {
                                    "type": "string",
                                    "enum": ["high", "medium", "low"],
                                    "description": "Relative importance of the task.",
                                    "default": "medium"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Execution status for this task.",
                                    "default": "pending"
                                },
                                "_meta": {
                                    "description": "Optional metadata to store with the plan item."
                                }
                            },
                            "required": ["content"]
                        },
                        "default": []
                    },
                    "_meta": {
                        "description": "Optional metadata applied to the entire plan."
                    }
                },
                "required": ["entries"]
            }),
            annotations: None,
            supported_scopes: &[ToolScope::Agent, ToolScope::AgentWithDiffBlocks],
            hidden: true,
            title_template: Some("Updating plan ({entries} items)"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let new_plan = Self::build_plan(&input.entries, input.meta.clone())?;

        let (counts, plan_snapshot) = {
            let plan_ref = context
                .plan
                .as_deref_mut()
                .ok_or_else(|| anyhow!("Plan state is unavailable in this context"))?;
            *plan_ref = new_plan;

            let counts = Self::compute_counts(plan_ref);
            let snapshot = plan_ref.clone();
            (counts, snapshot)
        };

        let summary = if plan_snapshot.entries.is_empty() {
            "Plan cleared".to_string()
        } else {
            format!(
                "Plan updated with {} item(s)",
                plan_snapshot.entries.len()
            )
        };

        if let Some(ui) = context.ui {
            ui.send_event(UiEvent::UpdatePlan {
                plan: plan_snapshot.clone(),
            })
            .await?;
        }

        Ok(UpdatePlanOutput { summary, counts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::ToolTestFixture;
    use crate::ui::UiEvent;

    #[tokio::test]
    async fn test_update_plan_applies_entries() {
        let tool = UpdatePlanTool;
        let mut fixture = ToolTestFixture::new()
            .with_plan()
            .with_ui()
            .with_tool_id("plan-tool".into());
        let mut context = fixture.context();

        let mut input = UpdatePlanInput {
            entries: vec![
                PlanEntryInput {
                    content: "Review PR #123".into(),
                    priority: PlanItemPriority::High,
                    status: PlanItemStatus::Pending,
                    meta: None,
                },
                PlanEntryInput {
                    content: "Write unit tests".into(),
                    priority: PlanItemPriority::Medium,
                    status: PlanItemStatus::InProgress,
                    meta: None,
                },
            ],
            meta: None,
        };

        let output = tool.execute(&mut context, &mut input).await.unwrap();
        assert!(output.summary.contains("Plan updated"));
        assert_eq!(output.counts.pending, 1);
        assert_eq!(output.counts.in_progress, 1);
        assert_eq!(output.counts.completed, 0);

        let plan = fixture.plan().unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Review PR #123");

        let ui = fixture.ui().unwrap();
        let events = ui.events();
        assert!(events
            .iter()
            .any(|event| matches!(event, UiEvent::UpdatePlan { .. })));
    }

    #[tokio::test]
    async fn test_update_plan_requires_plan_context() {
        let tool = UpdatePlanTool;
        let mut fixture = ToolTestFixture::new();
        let mut context = fixture.context();
        let mut input = UpdatePlanInput::default();

        let err = tool.execute(&mut context, &mut input).await.unwrap_err();
        assert!(
            err.to_string().contains("Plan state is unavailable"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_update_plan_rejects_empty_content() {
        let tool = UpdatePlanTool;
        let mut fixture = ToolTestFixture::new().with_plan();
        let mut context = fixture.context();

        let mut input = UpdatePlanInput {
            entries: vec![PlanEntryInput {
                content: "   ".into(),
                priority: PlanItemPriority::Low,
                status: PlanItemStatus::Completed,
                meta: None,
            }],
            meta: None,
        };

        let err = tool.execute(&mut context, &mut input).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Plan entries must include non-empty content"));
    }
}
