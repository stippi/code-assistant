//! Below the title bar: an MCP server's login expired and logging in again
//! means switching to the browser — go ahead? Shown while the core waits
//! for the answer (`UiEvent::McpLoginsRequested`).

use super::MainScreen;
use crate::Gpui;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::{AnyElement, Context, SharedString, div, prelude::*, px};

/// The question for the first waiting server, and how many more wait.
pub(super) fn message(servers: &[String]) -> Option<String> {
    let (first, rest) = servers.split_first()?;
    let more = match rest.len() {
        0 => String::new(),
        1 => " One more server waits as well.".to_string(),
        n => format!(" {n} more servers wait as well."),
    };
    Some(format!(
        "The login of MCP server '{first}' expired. Log in again in the browser?{more}"
    ))
}

impl MainScreen {
    pub(super) fn render_mcp_login_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let servers = cx.try_global::<Gpui>()?.get_mcp_login_requests();
        let text = message(&servers)?;
        let server = servers.first()?.clone();
        let decline = server.clone();
        Some(
            div()
                .id("mcp-login-prompt")
                .absolute()
                .top(px(52.))
                .right(px(16.))
                .w(px(320.))
                .p_3()
                .flex()
                .flex_col()
                .gap_3()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .shadow_lg()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(SharedString::from(text)),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("mcp-login-later")
                                .label("Not now")
                                .small()
                                .ghost()
                                .on_click(move |_, _, cx| {
                                    if let Some(gpui) = cx.try_global::<Gpui>() {
                                        gpui.cmd_answer_mcp_login(decline.clone(), false);
                                    }
                                }),
                        )
                        .child(
                            Button::new("mcp-login-go")
                                .label("Log in")
                                .small()
                                .primary()
                                .on_click(move |_, _, cx| {
                                    if let Some(gpui) = cx.try_global::<Gpui>() {
                                        gpui.cmd_answer_mcp_login(server.clone(), true);
                                    }
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::message;

    #[test]
    fn asks_about_the_first_server_and_counts_the_rest() {
        assert_eq!(message(&[]), None);
        let one = vec!["jira".to_string()];
        assert_eq!(
            message(&one).as_deref(),
            Some("The login of MCP server 'jira' expired. Log in again in the browser?")
        );
        let three = vec!["jira".into(), "wiki".into(), "docs".into()];
        assert!(
            message(&three)
                .unwrap()
                .ends_with(" 2 more servers wait as well.")
        );
    }
}
