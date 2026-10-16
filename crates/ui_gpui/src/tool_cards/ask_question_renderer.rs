//! Inline renderer for `ask_question`.
//!
//! The questions themselves are answered in the form above the input area;
//! in the transcript the call is one line naming what was asked. Expanding
//! it shows the answers (or that the user declined).

use super::inline_renderer::render_inline_output;
use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use code_assistant_core::session::questions::UserQuestion;
use gpui_kit::{AnyElement, Context, Window};

pub struct AskQuestionRenderer;

impl ToolBlockRenderer for AskQuestionRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["ask_question".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        match questions(tool).as_deref() {
            Some([question]) => format!("Ask \"{}\"", question.question.trim()),
            Some(questions) if questions.len() > 1 => {
                format!("Ask {} questions", questions.len())
            }
            // Parameters still streaming.
            _ => "Ask the user".to_string(),
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_kit::component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        render_inline_output(tool, theme)
    }
}

/// The `questions` parameter, once it parses completely.
fn questions(tool: &ToolUseBlock) -> Option<Vec<UserQuestion>> {
    let value = &tool
        .parameters
        .iter()
        .find(|p| p.name == "questions")?
        .value;
    serde_json::from_str(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::tests::make_tool;

    #[test]
    fn single_question_is_quoted() {
        let tool = make_tool(
            "ask_question",
            &[(
                "questions",
                r#"[{"question":"Which db?","options":[{"label":"A"},{"label":"B"}]}]"#,
            )],
        );
        assert_eq!(AskQuestionRenderer.describe(&tool), "Ask \"Which db?\"");
    }

    #[test]
    fn several_questions_are_counted() {
        let q = r#"{"question":"Q?","options":[{"label":"A"},{"label":"B"}]}"#;
        let tool = make_tool("ask_question", &[("questions", &format!("[{q},{q}]"))]);
        assert_eq!(AskQuestionRenderer.describe(&tool), "Ask 2 questions");
    }

    #[test]
    fn streaming_parameters_fall_back() {
        let partial = make_tool("ask_question", &[("questions", r#"[{"question":"Wh"#)]);
        assert_eq!(AskQuestionRenderer.describe(&partial), "Ask the user");
        let bare = make_tool("ask_question", &[]);
        assert_eq!(AskQuestionRenderer.describe(&bare), "Ask the user");
    }
}
