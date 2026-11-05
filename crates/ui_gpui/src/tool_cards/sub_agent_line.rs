//! A `spawn_agent` call in the conversation: one line saying the sub-agent
//! runs, finished or failed. What it does while it runs shows in the
//! running agents panel above the input, not here; expanded, the line shows
//! its answer and a link to its transcript.

use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use code_assistant_core::agent::sub_agent::{SubAgentActivity, SubAgentOutput};
use code_assistant_core::ui::ToolStatus;
use gpui_kit::component::text::TextView;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Element, InteractiveElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, rems,
};

pub struct SubAgentLineRenderer;

fn param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

fn output(tool: &ToolUseBlock) -> Option<SubAgentOutput> {
    tool.output.as_deref().and_then(SubAgentOutput::from_json)
}

/// The first line of the instructions.
fn label(tool: &ToolUseBlock) -> &str {
    param(tool, "instructions")
        .and_then(|i| i.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or_default()
}

impl ToolBlockRenderer for SubAgentLineRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["spawn_agent".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        let state = match output(tool).and_then(|o| o.activity) {
            Some(SubAgentActivity::Completed) => "finished",
            Some(SubAgentActivity::Failed) => "failed",
            Some(SubAgentActivity::Cancelled) => "cancelled",
            _ if tool.status == ToolStatus::Error => "failed",
            _ => "running",
        };
        // The state comes first: the row cuts a long label at its width.
        match label(tool) {
            "" => format!("Sub-agent {state}"),
            label => format!("Sub-agent {state} · {label}"),
        }
    }

    fn header_tag(&self, tool: &ToolUseBlock) -> Option<String> {
        output(tool)?.agent_id.map(|id| format!("#{id}"))
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
        let output = output(tool)?;
        let text = output
            .error
            .clone()
            .or_else(|| output.response.clone())
            .filter(|t| !t.trim().is_empty());
        let transcript = output.session_id.clone();
        if text.is_none() && transcript.is_none() {
            return None;
        }
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let mut body = div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .flex()
            .flex_col()
            .gap_1()
            .text_size(rems(0.8125))
            .text_color(if output.error.is_some() {
                theme.danger
            } else {
                foreground.opacity(0.85)
            });
        if let Some(text) = text {
            body = body.child(
                TextView::markdown(SharedString::from(format!("sub-agent-{}", tool.id)), text)
                    .selectable(true),
            );
        }
        if let Some(session_id) = transcript {
            body = body.child(
                div()
                    .id(SharedString::from(format!("sub-agent-open-{}", tool.id)))
                    .text_size(rems(0.75))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(foreground))
                    .on_click(move |_: &ClickEvent, _window, cx| {
                        if let Some(gpui) = cx.try_global::<crate::Gpui>() {
                            gpui.cmd_load_session(session_id.clone(), None);
                        }
                    })
                    .child("Open transcript"),
            );
        }
        Some(body.into_any())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::tests::make_tool;

    #[test]
    fn a_sub_agent_reads_as_one_line_with_its_state() {
        let mut tool = make_tool(
            "spawn_agent",
            &[
                ("instructions", "\nFind the parser\n\nThen explain it."),
                ("background", "true"),
            ],
        );
        assert_eq!(
            SubAgentLineRenderer.describe(&tool),
            "Sub-agent running · Find the parser"
        );
        assert_eq!(SubAgentLineRenderer.header_tag(&tool), None);

        let mut done = SubAgentOutput::new();
        done.activity = Some(SubAgentActivity::Completed);
        done.agent_id = Some(7);
        tool.output = Some(done.to_json());
        assert_eq!(
            SubAgentLineRenderer.describe(&tool),
            "Sub-agent finished · Find the parser"
        );
        assert_eq!(
            SubAgentLineRenderer.header_tag(&tool).as_deref(),
            Some("#7")
        );

        tool.status = ToolStatus::Error;
        tool.output = None;
        assert_eq!(
            SubAgentLineRenderer.describe(&tool),
            "Sub-agent failed · Find the parser"
        );
    }

    #[test]
    fn no_instructions_leave_just_the_state() {
        let tool = make_tool("spawn_agent", &[]);
        assert_eq!(SubAgentLineRenderer.describe(&tool), "Sub-agent running");
    }
}
