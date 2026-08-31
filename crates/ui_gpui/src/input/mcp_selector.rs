//! Input-bar control for toggling MCP servers on/off for the current session.
//!
//! A small trigger in the selector row opens a popover listing the servers
//! available to the session (global `mcp-servers.json` plus the project's
//! trusted `.mcp.json`); each has a switch that enables/disables it for this
//! session only. The actual list and enabled state are pushed in from the
//! session snapshot via [`Self::set_servers`]; toggling emits an event the
//! `InputArea` forwards to the backend.

use code_assistant_core::ui::ui_events::McpServerToggle;
use gpui::{Context, EventEmitter, Render, SharedString, Window, deferred, div, prelude::*, px};
use gpui_component::{ActiveTheme, Icon, Sizable, Size, switch::Switch};

#[derive(Clone, Debug)]
pub enum McpSelectorEvent {
    /// The user flipped a server's switch. `enabled == false` deactivates it
    /// for the session.
    ServerToggled { name: String, enabled: bool },
}

pub struct McpSelector {
    servers: Vec<McpServerToggle>,
    open: bool,
}

impl EventEmitter<McpSelectorEvent> for McpSelector {}

impl McpSelector {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            servers: Vec::new(),
            open: false,
        }
    }

    /// Replace the server list (from the session snapshot). Closes the popover
    /// if the session no longer has any MCP servers.
    pub fn set_servers(&mut self, servers: Vec<McpServerToggle>, cx: &mut Context<Self>) {
        if self.servers != servers {
            self.servers = servers;
            if self.servers.is_empty() {
                self.open = false;
            }
            cx.notify();
        }
    }
}

impl Render for McpSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // No servers → render nothing so the control disappears from the bar.
        if self.servers.is_empty() {
            return div().into_any_element();
        }

        let total = self.servers.len();
        let enabled_count = self.servers.iter().filter(|s| s.enabled).count();
        let is_open = self.open;

        let trigger = div()
            .id("mcp-selector-trigger")
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
            .child(SharedString::from(format!("MCP {enabled_count}/{total}")))
            .child(
                Icon::default()
                    .path("icons/chevron_up_down.svg")
                    .with_size(Size::XSmall)
                    .text_color(cx.theme().muted_foreground),
            )
            .on_click(cx.listener(|this, _, _window, cx| {
                this.open = !this.open;
                cx.notify();
            }));

        let mut root = div().relative().flex().child(trigger);

        if is_open {
            // Owned copy so the per-row closures don't borrow `self`.
            let servers = self.servers.clone();
            // Deferred + occlude so the popover paints on the top layer above
            // the input-area chrome (e.g. the chat/input divider border) rather
            // than letting it bleed through, and swallows clicks behind it.
            let panel = deferred(
                div()
                    .occlude()
                    .absolute()
                    .bottom_full()
                    .right_0()
                    .mb_1()
                    .w(px(240.))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .shadow_lg()
                    .p_1()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(servers.into_iter().map(|server| {
                    let name = server.name.clone();
                    let switch_name = server.name.clone();
                    let toggle_name = server.name.clone();
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(cx.theme().foreground)
                                .truncate()
                                .child(SharedString::from(name)),
                        )
                        .child(
                            Switch::new(SharedString::from(format!("mcp-toggle-{switch_name}")))
                                .checked(server.enabled)
                                .with_size(Size::Small)
                                .on_click(cx.listener(
                                    move |this, new_value: &bool, _window, cx| {
                                        let new_value = *new_value;
                                        if let Some(s) =
                                            this.servers.iter_mut().find(|s| s.name == toggle_name)
                                        {
                                            s.enabled = new_value;
                                        }
                                        cx.emit(McpSelectorEvent::ServerToggled {
                                            name: toggle_name.clone(),
                                            enabled: new_value,
                                        });
                                        cx.notify();
                                    },
                                )),
                        )
                })));
            root = root.child(panel);
        }

        root.into_any_element()
    }
}
