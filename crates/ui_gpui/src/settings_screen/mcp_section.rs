//! MCP servers settings section — one expandable card per configured server
//! with live tool discovery and per-tool enable toggles. Backed by
//! `<config_dir>/mcp-servers.json`.
//!
//! The section edits the *raw* config (env values keep their `${VAR}`
//! placeholders); only the discovery connection resolves them. Servers are
//! connected when the app starts, so config changes apply after a restart.
//!
//! Servers that enabled plugins add are listed below the configured ones,
//! read-only: they change with the plugin (*Settings → Plugins*).

use code_assistant_core::tools::mcp::{
    self, DiscoveredTool, McpServerConfig, McpServersConfig, McpTransport,
};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, SharedString, div, prelude::*, px};
use std::collections::{HashMap, HashSet};
use tracing::warn;

#[derive(Clone, PartialEq)]
enum FormMode {
    Hidden,
    Adding,
    Editing(String),
}

/// Which transport the add/edit form is currently editing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormTransport {
    Stdio,
    Http,
}

/// Result of the async tool discovery for one server.
enum DiscoveryState {
    Loading,
    Loaded(Vec<DiscoveredTool>),
    Failed(String),
    /// The HTTP server requires OAuth and we have no (valid) token — offer an
    /// Authenticate action instead of a bare retry.
    NeedsAuth,
    /// The interactive OAuth login is running (browser open, awaiting the
    /// redirect callback).
    Authenticating,
}

/// A server an enabled plugin adds.
struct PluginServer {
    name: String,
    plugin: String,
    server: McpServerConfig,
}

pub struct McpSection {
    focus_handle: FocusHandle,
    config: McpServersConfig,
    plugin_servers: Vec<PluginServer>,
    /// Names of servers whose cards are expanded.
    expanded: HashSet<String>,
    /// Discovery state per server, filled lazily when a card is expanded.
    discovered: HashMap<String, DiscoveryState>,

    form_mode: FormMode,
    /// Transport selected in the add/edit form.
    form_transport: FormTransport,
    form_name_input: Entity<InputState>,
    form_command_input: Entity<InputState>,
    form_args_input: Entity<InputState>,
    form_env_input: Entity<TextareaState>,
    form_url_input: Entity<InputState>,
    form_headers_input: Entity<TextareaState>,
}

impl McpSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let form_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. jira"));
        let form_command_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. npx or /path/to/server"));
        let form_args_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("space-separated, e.g. -y my-mcp-server")
        });
        let form_env_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("one per line, e.g. API_TOKEN=${MY_TOKEN}")
        });
        let form_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. https://example.com/mcp"));
        let form_headers_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("one per line, e.g. Authorization=Bearer ${MY_TOKEN}")
        });
        Self {
            focus_handle: cx.focus_handle(),
            config: load_config(),
            plugin_servers: load_plugin_servers(),
            expanded: HashSet::new(),
            discovered: HashMap::new(),
            form_mode: FormMode::Hidden,
            form_transport: FormTransport::Stdio,
            form_name_input,
            form_command_input,
            form_args_input,
            form_env_input,
            form_url_input,
            form_headers_input,
        }
    }

    /// Reload the configuration from disk.
    pub fn reload(&mut self) {
        self.config = load_config();
        self.plugin_servers = load_plugin_servers();
        self.discovered.clear();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = mcp::save_mcp_servers_config(&self.config) {
            warn!("Failed to save mcp-servers.json: {e:#}");
        }
        cx.notify();
    }

    fn set_server_enabled(&mut self, name: &str, value: bool, cx: &mut Context<Self>) {
        if let Some(server) = self.config.servers.get_mut(name) {
            server.enabled = value;
            self.persist(cx);
        }
    }

    fn set_auto_reauthorize(&mut self, value: bool, cx: &mut Context<Self>) {
        self.config.auto_reauthorize = value;
        self.persist(cx);
    }

    fn set_server_off_in_new_sessions(&mut self, name: &str, value: bool, cx: &mut Context<Self>) {
        if let Some(server) = self.config.servers.get_mut(name) {
            server.off_in_new_sessions = value;
            self.persist(cx);
        }
    }

    fn toggle_tool(&mut self, server_name: &str, tool: &str, cx: &mut Context<Self>) {
        let Some(server) = self.config.servers.get_mut(server_name) else {
            return;
        };
        if server.is_tool_enabled(tool) {
            server.disabled_tools.push(tool.to_string());
        } else {
            server.disabled_tools.retain(|name| name != tool);
            // Hand-edited allowlists must gain the tool, or removing it from
            // the denylist would not actually enable it.
            if let Some(allowlist) = &mut server.enabled_tools
                && !allowlist.iter().any(|name| name == tool)
            {
                allowlist.push(tool.to_string());
            }
        }
        self.persist(cx);
    }

    fn toggle_expanded(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(name) {
            self.expanded.insert(name.to_string());
            if !self.discovered.contains_key(name) {
                self.start_discovery(name.to_string(), cx);
            }
        }
        cx.notify();
    }

    /// Connect to the server on a background thread (with a dedicated tokio
    /// runtime — gpui's executor is not tokio) and list its tools.
    fn start_discovery(&mut self, name: String, cx: &mut Context<Self>) {
        self.discovered
            .insert(name.clone(), DiscoveryState::Loading);
        cx.spawn(async move |this, cx| {
            let server_name = name.clone();
            let result = cx
                .background_spawn(async move {
                    // The runtime config resolves ${ENV_VAR} placeholders
                    // and includes the plugins' servers.
                    let server = mcp::resolve_mcp_server(&server_name, None)?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(mcp::discover_server_tools(&server_name, &server))
                })
                .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(tools) => DiscoveryState::Loaded(tools),
                    // An HTTP server that demands OAuth gets an Authenticate
                    // action instead of a plain failure.
                    Err(error) if error.downcast_ref::<mcp::AuthorizationRequired>().is_some() => {
                        DiscoveryState::NeedsAuth
                    }
                    Err(error) => DiscoveryState::Failed(format!("{error:#}")),
                };
                this.discovered.insert(name, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run the interactive OAuth login for `name` on a background thread, then
    /// re-run tool discovery so its tools appear on success (or the error
    /// shows on failure).
    fn start_authentication(&mut self, name: String, cx: &mut Context<Self>) {
        self.discovered
            .insert(name.clone(), DiscoveryState::Authenticating);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let server_name = name.clone();
            let result = cx
                .background_spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(code_assistant_core::tools::mcp_auth::login_mcp_server(
                        &server_name,
                        None,
                    ))
                })
                .await;
            this.update(cx, |this, cx| match result {
                // On success re-discover: the stored token now lets us list
                // the server's tools.
                Ok(()) => this.start_discovery(name, cx),
                Err(error) => {
                    this.discovered
                        .insert(name, DiscoveryState::Failed(format!("{error:#}")));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_add_form(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        self.form_mode = FormMode::Adding;
        self.fill_form("", None, window, cx);
        cx.notify();
    }

    fn open_edit_form(
        &mut self,
        name: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let server = self.config.servers.get(name).cloned();
        self.form_mode = FormMode::Editing(name.to_string());
        self.fill_form(name, server.as_ref(), window, cx);
        cx.notify();
    }

    fn fill_form(
        &mut self,
        name: &str,
        server: Option<&McpServerConfig>,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        // Pick the transport tab from the existing server, defaulting new
        // servers to stdio.
        self.form_transport = match server.map(|s| &s.transport) {
            Some(McpTransport::Http { .. }) => FormTransport::Http,
            _ => FormTransport::Stdio,
        };

        let (command, args, env) = match server.map(|s| &s.transport) {
            Some(McpTransport::Stdio { command, args, env }) => {
                (command.clone(), join_args(args), format_map(env))
            }
            _ => (String::new(), String::new(), String::new()),
        };
        let (url, headers) = match server.map(|s| &s.transport) {
            Some(McpTransport::Http { url, headers }) => (url.clone(), format_map(headers)),
            _ => (String::new(), String::new()),
        };

        self.form_name_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(name.to_string()), window, cx)
        });
        self.form_command_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(command), window, cx)
        });
        self.form_args_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(args), window, cx)
        });
        self.form_env_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(env), window, cx)
        });
        self.form_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(url), window, cx)
        });
        self.form_headers_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(headers), window, cx)
        });
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let name = self.form_name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            warn!("MCP server needs a name");
            return;
        }

        let transport = match self.form_transport {
            FormTransport::Stdio => {
                let command = self.form_command_input.read(cx).value().trim().to_string();
                if command.is_empty() {
                    warn!("A stdio MCP server needs a command");
                    return;
                }
                let Some(args) = split_args(&self.form_args_input.read(cx).value()) else {
                    warn!("The MCP server arguments have an unbalanced quote");
                    return;
                };
                let env = parse_map(&self.form_env_input.read(cx).value());
                McpTransport::Stdio { command, args, env }
            }
            FormTransport::Http => {
                let url = self.form_url_input.read(cx).value().trim().to_string();
                if url.is_empty() {
                    warn!("An HTTP MCP server needs a URL");
                    return;
                }
                let headers = parse_map(&self.form_headers_input.read(cx).value());
                McpTransport::Http { url, headers }
            }
        };

        // Renaming moves the entry (and its tool filter) to the new key.
        let previous = match &self.form_mode {
            FormMode::Editing(old_name) => self.config.servers.remove(old_name),
            _ => None,
        };
        let mut server = previous.unwrap_or_else(|| McpServerConfig {
            transport: McpTransport::stdio(String::new()),
            enabled: true,
            off_in_new_sessions: false,
            enabled_tools: None,
            disabled_tools: Vec::new(),
        });
        server.transport = transport;
        self.config.servers.insert(name.clone(), server);

        self.form_mode = FormMode::Hidden;
        self.discovered.remove(&name);
        self.persist(cx);
    }

    fn delete_server(&mut self, name: &str, cx: &mut Context<Self>) {
        self.config.servers.remove(name);
        self.expanded.remove(name);
        self.discovered.remove(name);
        self.form_mode = FormMode::Hidden;
        self.persist(cx);
    }

    fn render_server_card(
        &self,
        name: &str,
        server: &McpServerConfig,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_expanded = self.expanded.contains(name);
        let is_editing = self.form_mode == FormMode::Editing(name.to_string());
        let view = cx.entity();

        let name_for_expand = name.to_string();
        let name_for_switch = name.to_string();

        let summary = transport_summary(&server.transport);

        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            // Header row: chevron + name + command summary + enabled switch
            .child(
                div()
                    .id(SharedString::from(format!("mcp-server-header-{name}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.toggle_expanded(&name_for_expand, cx);
                    }))
                    .child(
                        Icon::default()
                            .path(SharedString::from(if is_expanded {
                                "icons/chevron_down.svg"
                            } else {
                                "icons/chevron_right.svg"
                            }))
                            .with_size(Size::Small)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(name.to_string())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(SharedString::from(summary)),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("mcp-server-enabled-{name}")))
                            .checked(server.enabled)
                            .with_size(Size::Small)
                            .on_click({
                                let view = view.clone();
                                move |new_value, _window, app| {
                                    let name = name_for_switch.clone();
                                    let new_value = *new_value;
                                    view.update(app, |this, cx| {
                                        this.set_server_enabled(&name, new_value, cx);
                                    });
                                }
                            }),
                    ),
            )
            // Expanded body
            .when(is_expanded && is_editing, |el| {
                el.child(self.render_inline_form(cx))
            })
            .when(is_expanded && !is_editing, |el| {
                el.child(self.render_card_body(name, server, cx))
            })
    }

    /// Expanded card body: edit/delete actions and the discovered tool list.
    fn render_card_body(
        &self,
        name: &str,
        server: &McpServerConfig,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name_for_edit = name.to_string();
        let name_for_delete = name.to_string();
        let name_for_new_sessions = name.to_string();
        let view = cx.entity();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_3()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            // New sessions: on (default) or off until switched on per session
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(
                        Switch::new(SharedString::from(format!(
                            "mcp-server-new-sessions-{name}"
                        )))
                        .checked(!server.off_in_new_sessions)
                        .with_size(Size::Small)
                        .on_click(move |on, _window, app| {
                            let name = name_for_new_sessions.clone();
                            let off = !*on;
                            view.update(app, |this, cx| {
                                this.set_server_off_in_new_sessions(&name, off, cx);
                            });
                        }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("On in new sessions"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "— unless the project remembers otherwise; switch it per \
                                 session in the input row",
                            ),
                    ),
            )
            // Action row
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("TOOLS"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("mcp-edit-{name}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child("Edit")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_edit_form(&name_for_edit, window, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("mcp-delete-{name}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(gpui_kit::hsla(0.0, 0.7, 0.5, 1.0))
                                    .hover(|s| s.bg(gpui_kit::hsla(0.0, 0.7, 0.5, 0.1)))
                                    .child("Delete")
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.delete_server(&name_for_delete, cx);
                                    })),
                            ),
                    ),
            )
            // Tool list by discovery state
            .child(self.render_discovery(name, Some(server), cx))
    }

    /// A server an enabled plugin adds: shown like a configured one, but
    /// without switches, edit or delete — it changes with its plugin.
    fn render_plugin_server_card(
        &self,
        entry: &PluginServer,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = entry.name.as_str();
        let is_expanded = self.expanded.contains(name);
        let name_for_expand = name.to_string();
        let shadows = self.config.servers.contains_key(name);
        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "mcp-plugin-server-header-{name}"
                    )))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.toggle_expanded(&name_for_expand, cx);
                    }))
                    .child(
                        Icon::default()
                            .path(SharedString::from(if is_expanded {
                                "icons/chevron_down.svg"
                            } else {
                                "icons/chevron_right.svg"
                            }))
                            .with_size(Size::Small)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(name.to_string())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(SharedString::from(transport_summary(
                                        &entry.server.transport,
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py(px(1.))
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!("plugin {}", entry.plugin))),
                    ),
            )
            .when(is_expanded, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .px_4()
                        .pb_3()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!(
                                    "Added by the plugin '{}' and not editable here. Switch it \
                                     per session in the input row, or turn the plugin off in \
                                     Settings → Plugins.{}",
                                    entry.plugin,
                                    if shadows {
                                        " It replaces the configured server of the same name."
                                    } else {
                                        ""
                                    }
                                ))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(cx.theme().muted_foreground)
                                .child("TOOLS"),
                        )
                        .child(self.render_discovery(name, None, cx)),
                )
            })
    }

    /// The server's tools by discovery state; `server` is `None` for a
    /// read-only (plugin) server, whose tools cannot be switched here.
    fn render_discovery(
        &self,
        name: &str,
        server: Option<&McpServerConfig>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let name_for_retry = name.to_string();
        let name_for_auth = name.to_string();
        match self.discovered.get(name) {
            None | Some(DiscoveryState::Loading) => div()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Connecting to server…")
                .into_any_element(),
            Some(DiscoveryState::Authenticating) => div()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Authenticating… complete the login in your browser.")
                .into_any_element(),
            Some(DiscoveryState::NeedsAuth) => div()
                .flex()
                .flex_col()
                .gap_2()
                .py_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("This server requires authorization."),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("mcp-auth-{name}")))
                        .self_start()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .hover(|s| s.opacity(0.9))
                        .child("Authenticate")
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.start_authentication(name_for_auth.clone(), cx);
                        })),
                )
                .into_any_element(),
            Some(DiscoveryState::Failed(error)) => div()
                .flex()
                .flex_col()
                .gap_2()
                .py_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_kit::hsla(0.0, 0.7, 0.5, 1.0))
                        .child(SharedString::from(format!("Connection failed: {error}"))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("mcp-retry-{name}")))
                        .self_start()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().muted))
                        .child("Retry")
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.start_discovery(name_for_retry.clone(), cx);
                            cx.notify();
                        })),
                )
                .into_any_element(),
            Some(DiscoveryState::Loaded(tools)) if tools.is_empty() => div()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("The server offers no tools.")
                .into_any_element(),
            Some(DiscoveryState::Loaded(tools)) => div()
                .flex()
                .flex_col()
                .children(
                    tools
                        .iter()
                        .map(|tool| self.render_tool_row(name, server, tool, cx)),
                )
                .into_any_element(),
        }
    }

    fn render_tool_row(
        &self,
        server_name: &str,
        server: Option<&McpServerConfig>,
        tool: &DiscoveredTool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let view = cx.entity();
        let enabled = server.map(|server| server.is_tool_enabled(&tool.name));
        let server_for_click = server_name.to_string();
        let tool_for_click = tool.name.clone();

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(tool.name.clone())),
                    )
                    .when(!tool.description.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(tool.description.clone())),
                        )
                    }),
            )
            .when_some(enabled, |el, enabled| {
                el.child(
                    Switch::new(SharedString::from(format!(
                        "mcp-tool-{server_name}-{}",
                        tool.name
                    )))
                    .checked(enabled)
                    .with_size(Size::Small)
                    .on_click(move |_new_value, _window, app| {
                        let server = server_for_click.clone();
                        let tool = tool_for_click.clone();
                        view.update(app, |this, cx| {
                            this.toggle_tool(&server, &tool, cx);
                        });
                    }),
                )
            })
    }

    /// The add/edit form: name, a transport selector, and the fields for the
    /// selected transport (command/args/env for stdio, url/headers for HTTP).
    fn render_inline_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing_name = match &self.form_mode {
            FormMode::Editing(name) => Some(name.clone()),
            _ => None,
        };
        let is_http = self.form_transport == FormTransport::Http;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.render_form_row(
                "Name",
                Input::new(&self.form_name_input).into_any_element(),
                cx,
            ))
            .child(self.render_form_row("Transport", self.render_transport_selector(cx), cx))
            .when(!is_http, |el| {
                el.child(self.render_form_row(
                    "Command",
                    Input::new(&self.form_command_input).into_any_element(),
                    cx,
                ))
                .child(self.render_form_row(
                    "Arguments",
                    Input::new(&self.form_args_input).into_any_element(),
                    cx,
                ))
                .child(self.render_form_row(
                    "Env",
                    Textarea::new(&self.form_env_input).into_any_element(),
                    cx,
                ))
            })
            .when(is_http, |el| {
                el.child(self.render_form_row(
                    "URL",
                    Input::new(&self.form_url_input).into_any_element(),
                    cx,
                ))
                .child(self.render_form_row(
                    "Headers",
                    Textarea::new(&self.form_headers_input).into_any_element(),
                    cx,
                ))
            })
            // Action buttons
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().when_some(editing_name, |el, name| {
                        el.child(
                            div()
                                .id("mcp-form-delete")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(gpui_kit::hsla(0.0, 0.7, 0.5, 1.0))
                                .hover(|s| s.bg(gpui_kit::hsla(0.0, 0.7, 0.5, 0.1)))
                                .child("Delete")
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.delete_server(&name, cx);
                                })),
                        )
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("mcp-form-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| s.bg(cx.theme().muted))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.form_mode = FormMode::Hidden;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("mcp-form-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                                    .hover(|s| s.opacity(0.9))
                                    .child("Save")
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.save_form(cx);
                                    })),
                            ),
                    ),
            )
    }

    /// Two toggle pills to pick the transport type in the add/edit form.
    fn render_transport_selector(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let pill = |label: &str,
                    id: &str,
                    selected: bool,
                    target: FormTransport,
                    cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(id.to_string()))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_xs()
                .when(selected, |s| {
                    s.bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                })
                .when(!selected, |s| {
                    s.text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().muted))
                })
                .child(SharedString::from(label.to_string()))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.form_transport = target;
                    cx.notify();
                }))
        };

        div()
            .flex()
            .items_center()
            .gap_2()
            .child(pill(
                "Command (stdio)",
                "mcp-transport-stdio",
                self.form_transport == FormTransport::Stdio,
                FormTransport::Stdio,
                cx,
            ))
            .child(pill(
                "HTTP",
                "mcp-transport-http",
                self.form_transport == FormTransport::Http,
                FormTransport::Http,
                cx,
            ))
            .into_any_element()
    }

    fn render_form_row(
        &self,
        label: &str,
        widget: gpui_kit::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(80.))
                    .flex_none()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(label.to_string())),
            )
            .child(div().flex_1().min_w_0().child(widget))
    }

    fn render_add_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("mcp-add-dialog-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(60.))
            .bg(cx.theme().background.opacity(0.6))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.form_mode = FormMode::Hidden;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("mcp-add-dialog")
                    .w(px(480.))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div().px_4().py_3().child(
                            div()
                                .text_base()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("New MCP Server"),
                        ),
                    )
                    .child(self.render_inline_form(cx)),
            )
    }
}

/// How a server is reached, in one line: its command line or URL.
fn transport_summary(transport: &McpTransport) -> String {
    match transport {
        McpTransport::Stdio { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{} {}", command, join_args(args))
            }
        }
        McpTransport::Http { url, .. } => url.clone(),
    }
}

fn load_plugin_servers() -> Vec<PluginServer> {
    let servers = code_assistant_core::plugin_host::catalog().mcp_servers();
    servers
        .config
        .servers
        .into_iter()
        .map(|(name, server)| PluginServer {
            plugin: servers.owners.get(&name).cloned().unwrap_or_default(),
            name,
            server,
        })
        .collect()
}

fn load_config() -> McpServersConfig {
    mcp::load_mcp_servers_config_raw().unwrap_or_else(|e| {
        warn!("Failed to load mcp-servers.json: {e:#}");
        McpServersConfig::default()
    })
}

/// Split the arguments input the way a shell would, so quotes group words
/// instead of ending up in the arguments. `None` for an unbalanced quote.
fn split_args(text: &str) -> Option<Vec<String>> {
    shlex::split(text)
}

/// Join arguments for the input, quoting those `split_args` would break up.
fn join_args(args: &[String]) -> String {
    shlex::try_join(args.iter().map(String::as_str)).unwrap_or_else(|_| args.join(" "))
}

/// Render a `KEY=value` map as one sorted `KEY=value` line per entry, for
/// display in a multi-line input.
fn format_map(map: &HashMap<String, String>) -> String {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort();
    entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a multi-line `KEY=value` input back into a map, ignoring blank lines.
fn parse_map(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

impl Focusable for McpSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for McpSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let servers: Vec<(String, McpServerConfig)> = self
            .config
            .servers
            .iter()
            .map(|(name, server)| (name.clone(), server.clone()))
            .collect();

        div()
            .relative()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            .mx_auto()
            // Header row with Add button
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child("MCP SERVERS"),
                    )
                    .child(
                        div()
                            .id("mcp-add-btn")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(cx.theme().primary)
                            .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/plus.svg"))
                                    .with_size(Size::XSmall)
                                    .text_color(cx.theme().primary_foreground),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().primary_foreground)
                                    .child("Add"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_form(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Tools of enabled servers are offered to the agent. Servers are \
                         connected when the app starts — changes apply after a restart.",
                    ),
            )
            // Renewing expired OAuth logins of HTTP servers
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new("mcp-auto-reauthorize")
                            .checked(self.config.auto_reauthorize)
                            .with_size(Size::Small)
                            .on_click(cx.listener(|this, on: &bool, _window, cx| {
                                this.set_auto_reauthorize(*on, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("Log in again when a login expires"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("— opens the browser for HTTP servers that use OAuth"),
                    ),
            )
            // Server cards / empty state
            .when(servers.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .py_8()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No MCP servers configured"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!(
                                    "Configured servers are stored in {}",
                                    mcp::mcp_servers_config_path().display()
                                ))),
                        ),
                )
            })
            .when(!servers.is_empty(), |el| {
                el.child(div().flex().flex_col().gap_2().children(servers.iter().map(
                    |(name, server)| self.render_server_card(name, server, cx).into_any_element(),
                )))
            })
            .when(!self.plugin_servers.is_empty(), |el| {
                el.child(
                    div()
                        .pt_2()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("FROM PLUGINS"),
                )
                .child(
                    div().flex().flex_col().gap_2().children(
                        self.plugin_servers.iter().map(|entry| {
                            self.render_plugin_server_card(entry, cx).into_any_element()
                        }),
                    ),
                )
            })
            // Add dialog overlay
            .when(self.form_mode == FormMode::Adding, |el| {
                el.child(self.render_add_dialog(cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_args_strips_shell_quotes() {
        let args = split_args(r#"--index "https://example.com/simple" "tam-mcp@0.2.3""#);
        assert_eq!(
            args,
            Some(vec![
                "--index".to_string(),
                "https://example.com/simple".to_string(),
                "tam-mcp@0.2.3".to_string(),
            ])
        );
    }

    #[test]
    fn split_args_rejects_unbalanced_quotes() {
        assert_eq!(split_args(r#"--index "https://example.com"#), None);
    }

    #[test]
    fn join_args_round_trips_arguments_with_spaces() {
        let args = vec!["--name".to_string(), "two words".to_string()];
        let text = join_args(&args);
        assert_eq!(text, "--name 'two words'");
        assert_eq!(split_args(&text), Some(args));
    }
}
