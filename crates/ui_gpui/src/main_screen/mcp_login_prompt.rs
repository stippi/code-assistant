//! Above the input: an MCP server's login expired and logging in again
//! means switching to the browser — go ahead? Shown while the core waits
//! for the answer (`UiEvent::McpLoginsRequested`), styled like the
//! permission prompts next to it.

use super::MainScreen;
use crate::Gpui;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{AnyElement, Context, SharedString, div, prelude::*};

/// The title for the first waiting server, and the line under it.
pub(super) fn message(servers: &[String]) -> Option<(String, String)> {
    let (first, rest) = servers.split_first()?;
    let more = match rest.len() {
        0 => String::new(),
        1 => " One more server waits as well.".to_string(),
        n => format!(" {n} more servers wait as well."),
    };
    Some((
        format!("Login expired: {first} MCP server"),
        format!("Logging in again opens the browser.{more}"),
    ))
}

impl MainScreen {
    pub(super) fn render_mcp_login_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let servers = cx.try_global::<Gpui>()?.get_mcp_login_requests();
        let (title, detail) = message(&servers)?;
        let server = servers.first()?.clone();
        let answer = move |ok: bool| {
            let server = server.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                if let Some(gpui) = cx.try_global::<Gpui>() {
                    gpui.cmd_answer_mcp_login(server.clone(), ok);
                }
            }
        };
        Some(
            div()
                .flex_none()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().warning.opacity(0.08))
                .p_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .truncate()
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(detail)),
                        ),
                )
                .child(
                    Self::prompt_button("mcp-login-go".into(), "Log in".into(), true, cx)
                        .on_click(answer(true)),
                )
                .child(
                    Self::prompt_button("mcp-login-later".into(), "Not now".into(), false, cx)
                        .on_click(answer(false)),
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
            message(&one),
            Some((
                "Login expired: jira MCP server".to_string(),
                "Logging in again opens the browser.".to_string()
            ))
        );
        let three = vec!["jira".into(), "wiki".into(), "docs".into()];
        assert!(
            message(&three)
                .unwrap()
                .1
                .ends_with(" 2 more servers wait as well.")
        );
    }
}
