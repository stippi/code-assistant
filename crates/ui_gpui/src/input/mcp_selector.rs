//! Input-bar control for toggling tools on/off for the current session.
//!
//! A small trigger in the selector row opens a popover listing the built-in
//! tool groups (browser, web, terminal, …) and the MCP servers available to
//! the session (global `mcp-servers.json` plus the project's trusted
//! `.mcp.json`); each has a switch that enables/disables it for this session
//! (and the project's next sessions). The lists and their state are pushed in
//! from the session snapshot via [`Self::set_servers`] /
//! [`Self::set_builtin_tools`]; toggling emits an event the `InputArea`
//! forwards to the backend.
//!
//! Each MCP server also shows its connection state (a dot, the error on
//! hover; see [`Self::set_statuses`]) and a restart button, available
//! whatever the state — or, while the server waits for an OAuth login, a
//! *Log in* button instead.

use code_assistant_core::ui::ui_events::{BuiltinToolToggle, McpServerHealth, McpServerToggle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, switch::Switch};
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, Hsla, Render, SharedString, Window, deferred, div,
    prelude::*, px,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum McpSelectorEvent {
    /// The user flipped a server's switch. `enabled == false` deactivates it
    /// for the session.
    ServerToggled { name: String, enabled: bool },
    /// The user flipped a built-in tool group's switch.
    BuiltinToolToggled { id: String, enabled: bool },
    /// The user asked to restart a server.
    RestartRequested { name: String },
    /// The user asked to log a server in that waits for an OAuth login.
    LoginRequested { name: String },
    /// The popover opened; the server states should be refreshed.
    Opened,
}

pub struct McpSelector {
    servers: Vec<McpServerToggle>,
    /// Connection state by server name; a missing server is not connected.
    statuses: BTreeMap<String, McpServerHealth>,
    builtin_tools: Vec<BuiltinToolToggle>,
    open: bool,
}

impl EventEmitter<McpSelectorEvent> for McpSelector {}

impl McpSelector {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            servers: Vec::new(),
            statuses: BTreeMap::new(),
            builtin_tools: Vec::new(),
            open: false,
        }
    }

    /// Replace the server list (from the session snapshot).
    pub fn set_servers(&mut self, servers: Vec<McpServerToggle>, cx: &mut Context<Self>) {
        if self.servers != servers {
            self.servers = servers;
            self.close_if_empty();
            cx.notify();
        }
    }

    /// Replace the built-in tool group list (from the session snapshot).
    pub fn set_builtin_tools(&mut self, groups: Vec<BuiltinToolToggle>, cx: &mut Context<Self>) {
        if self.builtin_tools != groups {
            self.builtin_tools = groups;
            self.close_if_empty();
            cx.notify();
        }
    }

    /// Replace the connection states (app-wide, by server name).
    pub fn set_statuses(
        &mut self,
        statuses: BTreeMap<String, McpServerHealth>,
        cx: &mut Context<Self>,
    ) {
        if self.statuses != statuses {
            self.statuses = statuses;
            cx.notify();
        }
    }

    /// Whether a server switched on for this session is failing.
    fn any_enabled_failing(&self) -> bool {
        self.servers.iter().any(|server| {
            server.enabled
                && matches!(
                    self.statuses.get(&server.name),
                    Some(McpServerHealth::Failed { .. } | McpServerHealth::NeedsLogin)
                )
        })
    }

    fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.builtin_tools.is_empty()
    }

    fn close_if_empty(&mut self) {
        if self.is_empty() {
            self.open = false;
        }
    }
}

/// The dot color and hover text for a server's state.
fn health_display(health: Option<&McpServerHealth>, cx: &App) -> (Hsla, SharedString) {
    match health {
        Some(McpServerHealth::Connected { tools }) => (
            cx.theme().success,
            format!(
                "Connected · {tools} tool{}",
                if *tools == 1 { "" } else { "s" }
            )
            .into(),
        ),
        Some(McpServerHealth::Connecting) => (cx.theme().warning, "Connecting…".into()),
        Some(McpServerHealth::Failed { error }) => (cx.theme().danger, error.clone().into()),
        Some(McpServerHealth::NeedsLogin) => (
            cx.theme().danger,
            "Login required — log in to use its tools".into(),
        ),
        None => (
            cx.theme().muted_foreground.opacity(0.5),
            "Not connected — starts with the next agent run".into(),
        ),
    }
}

/// The small square button next to a server's switch.
fn row_button(id: String, cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(SharedString::from(id))
        .flex_none()
        .h(px(18.))
        .min_w(px(18.))
        .rounded_sm()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().muted))
}

fn status_dot(color: Hsla) -> gpui_kit::Div {
    div().flex_none().size(px(7.)).rounded_full().bg(color)
}

fn section_header(label: &'static str, cx: &App) -> AnyElement {
    div()
        .px_2()
        .pt_1()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

/// One labeled row with a switch.
fn toggle_row(label: String, switch: Switch, cx: &App) -> AnyElement {
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
                .child(SharedString::from(label)),
        )
        .child(switch)
        .into_any_element()
}

impl Render for McpSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Nothing to switch → render nothing so the control disappears.
        if self.is_empty() {
            return div().into_any_element();
        }

        let total = self.servers.len() + self.builtin_tools.len();
        let enabled_count = self.servers.iter().filter(|s| s.enabled).count()
            + self.builtin_tools.iter().filter(|g| g.enabled).count();
        let is_open = self.open;

        let trigger = div()
            .id("mcp-selector-trigger")
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(self.any_enabled_failing(), |el| {
                        el.child(status_dot(cx.theme().danger))
                    })
                    .child(SharedString::from(format!("Tools {enabled_count}/{total}"))),
            )
            .child(
                Icon::default()
                    .path("icons/chevron_up_down.svg")
                    .with_size(Size::XSmall)
                    .text_color(cx.theme().muted_foreground),
            )
            .on_click(cx.listener(|this, _, _window, cx| {
                this.open = !this.open;
                if this.open {
                    cx.emit(McpSelectorEvent::Opened);
                }
                cx.notify();
            }));

        let mut root = div().relative().w_full().flex().child(trigger);

        if is_open {
            // Owned copies so the per-row closures don't borrow `self`.
            let servers = self.servers.clone();
            let builtin_tools = self.builtin_tools.clone();

            let mut rows: Vec<AnyElement> = Vec::new();
            if !builtin_tools.is_empty() {
                rows.push(section_header("BUILT-IN", cx));
            }
            for group in builtin_tools {
                let id = group.id.clone();
                let switch = Switch::new(SharedString::from(format!("builtin-toggle-{id}")))
                    .checked(group.enabled)
                    .with_size(Size::Small)
                    .on_click(cx.listener(move |this, new_value: &bool, _window, cx| {
                        let new_value = *new_value;
                        if let Some(g) = this.builtin_tools.iter_mut().find(|g| g.id == id) {
                            g.enabled = new_value;
                        }
                        cx.emit(McpSelectorEvent::BuiltinToolToggled {
                            id: id.clone(),
                            enabled: new_value,
                        });
                        cx.notify();
                    }));
                rows.push(toggle_row(group.label, switch, cx));
            }
            if !servers.is_empty() {
                rows.push(section_header("MCP SERVERS", cx));
            }
            for server in servers {
                let name = server.name.clone();
                let switch_name = server.name.clone();
                let toggle_name = server.name.clone();
                let restart_name = server.name.clone();
                let health = self.statuses.get(&server.name);
                let needs_login = health == Some(&McpServerHealth::NeedsLogin);
                let (dot_color, status_text) = health_display(health, cx);
                let action = if needs_login {
                    row_button(format!("mcp-login-{restart_name}"), cx)
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .tooltip(|window, cx| {
                            Tooltip::new("Log in through the browser").build(window, cx)
                        })
                        .child("Log in")
                        .on_click(cx.listener(move |_this, _, _window, cx| {
                            cx.emit(McpSelectorEvent::LoginRequested {
                                name: restart_name.clone(),
                            });
                        }))
                } else {
                    row_button(format!("mcp-restart-{restart_name}"), cx)
                        .tooltip(|window, cx| Tooltip::new("Restart server").build(window, cx))
                        .child(
                            Icon::default()
                                .path("icons/rotate_cw.svg")
                                .with_size(Size::XSmall)
                                .text_color(cx.theme().muted_foreground),
                        )
                        .on_click(cx.listener(move |_this, _, _window, cx| {
                            cx.emit(McpSelectorEvent::RestartRequested {
                                name: restart_name.clone(),
                            });
                        }))
                };
                let row = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .id(SharedString::from(format!("mcp-status-{name}")))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .tooltip(move |window, cx| {
                                Tooltip::new(status_text.clone()).build(window, cx)
                            })
                            .child(status_dot(dot_color))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .truncate()
                                    .child(SharedString::from(name)),
                            ),
                    )
                    .child(action)
                    .child(
                        Switch::new(SharedString::from(format!("mcp-toggle-{switch_name}")))
                            .checked(server.enabled)
                            .with_size(Size::Small)
                            .on_click(cx.listener(move |this, new_value: &bool, _window, cx| {
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
                            })),
                    )
                    .into_any_element();
                rows.push(row);
            }

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
                    .w(px(260.))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .shadow_lg()
                    .p_1()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(rows),
            );
            root = root.child(panel);
        }

        root.into_any_element()
    }
}
