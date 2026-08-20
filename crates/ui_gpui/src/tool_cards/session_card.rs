//! Inline renderer for `search_sessions` and `get_session_content` tool
//! blocks.
//!
//! Both tools emit structured JSON from `render_for_ui`; this renderer parses
//! it and lays it out with the same collapsible, left-bordered inline style as
//! `read_files` / `search_files` (see [`super::code_card`]).

use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle};
use crate::blocks::{BlockView, ToolUseBlock};
use code_assistant_core::ui::ToolStatus;
use gpui::{
    AnyElement, Context, Element, FontWeight, ParentElement, Styled, Window, div, px, rems,
};
use serde_json::Value;

pub struct SessionCardRenderer;

impl ToolBlockRenderer for SessionCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "search_sessions".to_string(),
            "get_session_content".to_string(),
        ]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Inline
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        match tool.name.as_str() {
            "search_sessions" => match get_param(tool, "project") {
                Some(project) if !project.is_empty() => {
                    format!("Search sessions in {project}")
                }
                _ => "Search sessions".to_string(),
            },
            "get_session_content" => match get_param(tool, "session_id") {
                Some(id) if !id.is_empty() => format!("Read session {id}"),
                _ => "Read session".to_string(),
            },
            other => other.replace('_', " "),
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        _card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        _cx: &mut Context<BlockView>,
    ) -> Option<AnyElement> {
        let output = tool.output.as_deref().unwrap_or("");
        if output.is_empty() {
            return None;
        }

        if let Ok(json) = serde_json::from_str::<Value>(output) {
            match json.get("kind").and_then(|k| k.as_str()) {
                Some("search_sessions") => return render_search_sessions(&json, theme),
                Some("get_session_content") => return render_session_content(&json, theme),
                _ => {}
            }
        }

        // Fallback: plain text (old sessions / errors).
        render_plain(output, tool.status == ToolStatus::Error, theme)
    }
}

// ---------------------------------------------------------------------------
// search_sessions
// ---------------------------------------------------------------------------

fn render_search_sessions(
    json: &Value,
    theme: &gpui_component::theme::Theme,
) -> Option<AnyElement> {
    let sessions = json.get("sessions").and_then(|s| s.as_array())?;
    let total = json.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
    let truncated = json
        .get("truncated")
        .and_then(|t| t.as_bool())
        .unwrap_or(false);

    if sessions.is_empty() {
        return Some(container(
            theme,
            vec![muted_line(
                theme,
                "No sessions matched the query.".to_string(),
            )],
        ));
    }

    let mut children: Vec<AnyElement> = Vec::new();

    let header = if truncated {
        format!("Found {} session(s) (showing {})", total, sessions.len())
    } else {
        format!("Found {} session(s)", total)
    };
    children.push(muted_line(theme, header));

    for s in sessions {
        let id = s.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let project = s.get("project").and_then(|v| v.as_str()).unwrap_or("");
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let msg_count = s.get("message_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let updated = s.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = if name.is_empty() { "(unnamed)" } else { name };

        // Session header line (slightly emphasized).
        children.push(
            div()
                .w_full()
                .px_3()
                .pt_1()
                .text_color(theme.foreground)
                .child(format!(
                    "{id}  [{project}]  \"{display_name}\"  — {msg_count} msg, {}",
                    short_time(updated)
                ))
                .into_any(),
        );

        let calls = s
            .get("matched_calls")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let call_count = s
            .get("matched_call_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(calls.len() as u64);

        if calls.is_empty() {
            if call_count > 0 {
                children.push(indented(
                    theme,
                    format!("({call_count} matching tool call(s))"),
                ));
            }
        } else {
            for call in &calls {
                if let Some(line) = call.as_str() {
                    children.push(indented(theme, line.to_string()));
                }
            }
            if call_count as usize > calls.len() {
                children.push(indented(
                    theme,
                    format!("… and {} more", call_count as usize - calls.len()),
                ));
            }
        }
    }

    if truncated {
        children.push(hint(
            theme,
            "Narrow with project, tool_call.value (e.g. a glob), a time range, or limit.",
        ));
    }

    Some(container(theme, children))
}

// ---------------------------------------------------------------------------
// get_session_content
// ---------------------------------------------------------------------------

fn render_session_content(
    json: &Value,
    theme: &gpui_component::theme::Theme,
) -> Option<AnyElement> {
    let items = json.get("items").and_then(|i| i.as_array())?;
    let id = json
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let project = json.get("project").and_then(|v| v.as_str()).unwrap_or("");
    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let message_count = json
        .get("message_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let returned = json
        .get("returned_items")
        .and_then(|v| v.as_u64())
        .unwrap_or(items.len() as u64);
    let truncated = json
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let total_items = json
        .get("total_items")
        .and_then(|v| v.as_u64())
        .unwrap_or(returned);
    let display_name = if name.is_empty() { "(unnamed)" } else { name };

    let mut children: Vec<AnyElement> = Vec::new();
    children.push(muted_line(
        theme,
        format!(
            "{id}  [{project}]  \"{display_name}\"  — {returned} of {message_count} message(s)"
        ),
    ));

    for item in items {
        let idx = item
            .get("message_index")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let item_truncated = item
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tool_name = item.get("tool_name").and_then(|v| v.as_str());

        let (label, body) = match kind {
            "user_text" => (format!("[#{idx}] User"), text.to_string()),
            "assistant_text" => (format!("[#{idx}] Assistant"), text.to_string()),
            "thinking" => (format!("[#{idx}] Thinking"), text.to_string()),
            "tool_call" => {
                let name = tool_name.unwrap_or("tool");
                let args = item.get("tool_input").map(compact_json).unwrap_or_default();
                (format!("[#{idx}] → {name}"), args)
            }
            "tool_result" => {
                let name = tool_name.unwrap_or("tool");
                let status = match item.get("is_error").and_then(|v| v.as_bool()) {
                    Some(true) => "error",
                    _ => "ok",
                };
                (
                    format!("[#{idx}] {name} result ({status})"),
                    text.to_string(),
                )
            }
            _ => (format!("[#{idx}]"), text.to_string()),
        };

        // Label (muted, emphasized).
        children.push(
            div()
                .w_full()
                .px_3()
                .pt_1()
                .text_color(theme.muted_foreground.opacity(0.8))
                .font_weight(FontWeight(600.0))
                .child(label)
                .into_any(),
        );

        // Body text, line by line so long content wraps predictably.
        for line in body.lines() {
            children.push(
                div()
                    .w_full()
                    .px_3()
                    .text_color(theme.foreground)
                    .child(line.to_string())
                    .into_any(),
            );
        }
        if item_truncated {
            children.push(indented(theme, "…".to_string()));
        }
    }

    if truncated {
        children.push(hint(
            theme,
            &format!(
                "{} more item(s) not shown. Narrow with range, parts, tool_names, or max_chars_per_item.",
                total_items - returned
            ),
        ));
    }

    Some(container(theme, children))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The outer left-bordered inline container shared with `code_card`.
fn container(theme: &gpui_component::theme::Theme, children: Vec<AnyElement>) -> AnyElement {
    div()
        .pl(px(8.))
        .ml(px(8.))
        .border_l_2()
        .border_color(theme.border)
        .py(px(4.))
        .text_size(rems(0.8125))
        .overflow_hidden()
        .flex()
        .flex_col()
        .children(children)
        .into_any()
}

fn muted_line(theme: &gpui_component::theme::Theme, text: String) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .pb_0p5()
        .text_color(theme.muted_foreground.opacity(0.7))
        .child(text)
        .into_any()
}

fn indented(theme: &gpui_component::theme::Theme, text: String) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .pl_5()
        .text_color(theme.muted_foreground)
        .child(text)
        .into_any()
}

fn hint(theme: &gpui_component::theme::Theme, text: &str) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .pt_1()
        .text_color(theme.muted_foreground.opacity(0.5))
        .text_size(rems(0.75))
        .child(text.to_string())
        .into_any()
}

fn render_plain(
    output: &str,
    is_error: bool,
    theme: &gpui_component::theme::Theme,
) -> Option<AnyElement> {
    let color = if is_error {
        theme.danger
    } else {
        theme.muted_foreground
    };
    Some(
        div()
            .pl(px(8.))
            .ml(px(8.))
            .border_l_2()
            .border_color(theme.border)
            .py(px(4.))
            .text_size(rems(0.8125))
            .text_color(color)
            .overflow_hidden()
            .child(output.to_string())
            .into_any(),
    )
}

/// The date portion of an RFC 3339 timestamp (drop the time for compactness).
fn short_time(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

/// A compact one-line rendering of a JSON tool input.
fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn get_param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::{ParameterBlock, ToolUseBlock};
    use code_assistant_core::ui::ToolStatus;

    fn make_tool(name: &str, params: &[(&str, &str)]) -> ToolUseBlock {
        ToolUseBlock {
            name: name.to_string(),
            id: "test-id".to_string(),
            parameters: params
                .iter()
                .map(|(n, v)| ParameterBlock {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
            status: ToolStatus::Success,
            status_message: None,
            output: None,
            styled_output: None,
            state: crate::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn describe_search_sessions_with_project() {
        let r = SessionCardRenderer;
        assert_eq!(
            r.describe(&make_tool("search_sessions", &[("project", "mlflow")])),
            "Search sessions in mlflow"
        );
        assert_eq!(
            r.describe(&make_tool("search_sessions", &[])),
            "Search sessions"
        );
    }

    #[test]
    fn describe_get_session_content() {
        let r = SessionCardRenderer;
        assert_eq!(
            r.describe(&make_tool(
                "get_session_content",
                &[("session_id", "chat_x")]
            )),
            "Read session chat_x"
        );
    }

    #[test]
    fn short_time_keeps_date_only() {
        assert_eq!(short_time("2026-08-20T08:56:10.8+00:00"), "2026-08-20");
    }
}
