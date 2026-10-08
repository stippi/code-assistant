use crate::session::questions::{
    MAX_OPTIONS, MAX_QUESTIONS, MIN_OPTIONS, QuestionAnswer, QuestionOutcome, UserQuestion,
    UserQuestionRequest,
};
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use crate::tools::services::ToolServicesAccess;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

#[derive(Deserialize, Serialize)]
pub struct AskQuestionInput {
    pub questions: Vec<UserQuestion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskQuestionStatus {
    Answered,
    Declined,
    Cancelled,
}

#[derive(Serialize, Deserialize)]
pub struct AskQuestionOutput {
    pub questions: Vec<UserQuestion>,
    pub status: AskQuestionStatus,
    /// One answer per question when `status` is `Answered`.
    #[serde(default)]
    pub answers: Vec<QuestionAnswer>,
}

impl Render for AskQuestionOutput {
    fn status(&self) -> String {
        match self.status {
            AskQuestionStatus::Answered => "User answered".to_string(),
            AskQuestionStatus::Declined => "User declined to answer".to_string(),
            AskQuestionStatus::Cancelled => "Question cancelled".to_string(),
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        match self.status {
            AskQuestionStatus::Declined => {
                return "The user dismissed the questions without answering. \
                        Proceed with your best judgment or ask in plain text."
                    .to_string();
            }
            AskQuestionStatus::Cancelled => {
                return "The questions were cancelled before the user answered.".to_string();
            }
            AskQuestionStatus::Answered => {}
        }
        let mut out = String::from("The user answered:\n");
        for (question, answer) in self.questions.iter().zip(&self.answers) {
            out.push_str(&format!("\nQuestion: {}\n", question.question));
            if answer.selected.is_empty() {
                out.push_str("Selected: (none)\n");
            } else {
                out.push_str(&format!("Selected: {}\n", answer.selected.join(", ")));
            }
            if !answer.comment.trim().is_empty() {
                out.push_str(&format!("Comment: {}\n", answer.comment.trim()));
            }
        }
        out
    }
}

impl ToolResult for AskQuestionOutput {
    fn is_success(&self) -> bool {
        self.status == AskQuestionStatus::Answered
    }
}

/// Lets the agent ask the user up to four multiple-choice questions and wait
/// for the answers (selected options plus a free-text comment per question).
pub struct AskQuestionTool;

fn validate(questions: &[UserQuestion]) -> Result<()> {
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(anyhow!(
            "Provide between 1 and {MAX_QUESTIONS} questions (got {})",
            questions.len()
        ));
    }
    for (index, question) in questions.iter().enumerate() {
        let number = index + 1;
        if question.question.trim().is_empty() {
            return Err(anyhow!("Question {number} has no text"));
        }
        if question.options.len() < MIN_OPTIONS || question.options.len() > MAX_OPTIONS {
            return Err(anyhow!(
                "Question {number} must have between {MIN_OPTIONS} and {MAX_OPTIONS} options (got {})",
                question.options.len()
            ));
        }
        let mut labels = HashSet::new();
        for option in &question.options {
            let label = option.label.trim();
            if label.is_empty() {
                return Err(anyhow!("Question {number} has an option without a label"));
            }
            if !labels.insert(label) {
                return Err(anyhow!(
                    "Question {number} has duplicate option label '{label}'"
                ));
            }
        }
    }
    Ok(())
}

#[async_trait::async_trait]
impl Tool for AskQuestionTool {
    type Input = AskQuestionInput;
    type Output = AskQuestionOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Ask the user up to 4 multiple-choice questions and wait for the answers. ",
            "Use this when you are blocked on a decision only the user can make ",
            "(preferences, ambiguous requirements, choosing between approaches). ",
            "Each question offers 2-4 options; set multi_select to let the user pick several. ",
            "The user can always add a free-text comment, so do not add an 'Other' option. ",
            "If you recommend an option, list it first and append '(Recommended)' to its label. ",
            "Do not use this for questions you can answer yourself from the code or context."
        );
        ToolSpec {
            name: "ask_question".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "description": "The questions to ask (1-4)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {
                                    "type": "string",
                                    "description": "The complete question, ending with a question mark"
                                },
                                "header": {
                                    "type": "string",
                                    "description": "Very short label for the question (max ~12 characters)"
                                },
                                "multi_select": {
                                    "type": "boolean",
                                    "description": "Allow selecting several options (checkboxes) instead of one (radio buttons)",
                                    "default": false
                                },
                                "options": {
                                    "type": "array",
                                    "minItems": MIN_OPTIONS,
                                    "maxItems": MAX_OPTIONS,
                                    "description": "The choices (2-4)",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "type": "string",
                                                "description": "Concise option text (1-5 words)"
                                            },
                                            "description": {
                                                "type": "string",
                                                "description": "What this option means or implies"
                                            }
                                        },
                                        "required": ["label"]
                                    }
                                }
                            },
                            "required": ["question", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
            annotations: None,
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Asking the user"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        validate(&input.questions)?;
        let ui = context
            .ui()
            .ok_or_else(|| anyhow!("No user interface is available to ask questions"))?;

        let request = UserQuestionRequest::new(context.tool_id.clone(), input.questions.clone());
        let outcome = ui
            .ask_questions(request)
            .await
            .map_err(|e| anyhow!("Cannot ask the user: {e}"))?;

        let (status, answers) = match outcome {
            QuestionOutcome::Answered(mut answers) => {
                // Keep exactly one answer per question, and only known labels.
                answers.resize_with(input.questions.len(), QuestionAnswer::default);
                for (question, answer) in input.questions.iter().zip(answers.iter_mut()) {
                    answer
                        .selected
                        .retain(|label| question.options.iter().any(|o| &o.label == label));
                    if !question.multi_select {
                        answer.selected.truncate(1);
                    }
                }
                (AskQuestionStatus::Answered, answers)
            }
            QuestionOutcome::Declined => (AskQuestionStatus::Declined, Vec::new()),
            QuestionOutcome::Cancelled => (AskQuestionStatus::Cancelled, Vec::new()),
        };

        Ok(AskQuestionOutput {
            questions: input.questions.clone(),
            status,
            answers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::questions::QuestionOption;

    fn question(options: &[&str], multi_select: bool) -> UserQuestion {
        UserQuestion {
            question: "Which one?".to_string(),
            header: String::new(),
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: label.to_string(),
                    description: String::new(),
                })
                .collect(),
            multi_select,
        }
    }

    #[test]
    fn validate_enforces_limits() {
        assert!(validate(&[]).is_err());
        assert!(validate(&[question(&["A"], false)]).is_err());
        assert!(validate(&[question(&["A", "B", "C", "D", "E"], false)]).is_err());
        assert!(validate(&[question(&["A", "A"], false)]).is_err());
        assert!(validate(&vec![question(&["A", "B"], false); 5]).is_err());
        assert!(validate(&vec![question(&["A", "B"], true); 4]).is_ok());
    }

    #[test]
    fn input_parses_with_optional_fields() {
        let input: AskQuestionInput = serde_json::from_value(json!({
            "questions": [{
                "question": "Which db?",
                "options": [{"label": "Postgres"}, {"label": "SQLite", "description": "embedded"}]
            }]
        }))
        .unwrap();
        assert!(!input.questions[0].multi_select);
        assert_eq!(input.questions[0].options[1].description, "embedded");
    }

    #[test]
    fn render_lists_selections_and_comments() {
        let output = AskQuestionOutput {
            questions: vec![question(&["A", "B"], true)],
            status: AskQuestionStatus::Answered,
            answers: vec![QuestionAnswer {
                selected: vec!["A".to_string(), "B".to_string()],
                comment: "both please".to_string(),
            }],
        };
        let text = output.render(&mut ResourcesTracker::new());
        assert!(text.contains("Selected: A, B"));
        assert!(text.contains("Comment: both please"));
    }
}
