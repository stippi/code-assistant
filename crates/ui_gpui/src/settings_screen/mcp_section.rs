//! MCP servers settings section — one expandable card per configured server
//! with live tool discovery and per-tool enable toggles. Backed by
//! `<config_dir>/mcp-servers.json`.
//!
//! The section edits the *raw* config (env values keep their `${VAR}`
//! placeholders); only the discovery connection resolves them. Servers are
//! connected when the app starts, so config changes apply after a restart.

use code_assistant_core::tools::mcp::{self, DiscoveredTool, McpServerConfig, McpServersConfig};
use gpui::{div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, SharedString};
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Icon, Sizable, Size};
use std::collections::{HashMap, HashSet};
use tracing::warn;

#[derive(Clone, PartialEq)]
enum FormMode {
    Hidden,
    Adding,
    Editing(String),
}

/// Result of the async tool discovery for one server.
enum DiscoveryState {
    Loading,
    Loaded(Vec<DiscoveredTool>),
    Failed(String),
}

pub struct McpSection {
    focus_handle: FocusHandle,
    config: McpServersConfig,
    /// Names of servers whose cards are expanded.
    expanded: HashSet<String>,
    /// Discovery state per server, filled lazily when a card is expanded.
    discovered: HashMap<String, DiscoveryState>,

    form_mode: FormMode,
    form_name_input: Entity<InputState>,
    form_command_input: Entity<InputState>,
    form_args_input: Entity<InputState>,
    form_env_input: Entity<InputState>,
}

impl McpSection {
    pub fn new(window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let form_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. jira"));
        let form_command_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. npx or /path/to/server"));
        let form_args_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("space-separated, e.g. -y my-mcp-server")
        });
        let form_env_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 6)
                .placeholder("one per line, e.g. API_TOKEN=${MY_TOKEN}")
        });
        Self {
            focus_handle: cx.focus_handle(),
            config: load_config(),
            expanded: HashSet::new(),
            discovered: HashMap::new(),
            form_mode: FormMode::Hidden,
            form_name_input,
            form_command_input,
            form_args_input,
            form_env_input,
        }
    }

    /// Reload the configuration from disk.
    pub fn reload(&mut self) {
        self.config = load_config();
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
            if let Some(allowlist) = &mut server.enabled_tools {
                if !allowlist.iter().any(|name| name == tool) {
                    allowlist.push(tool.to_string());
                }
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
                    // The runtime config resolves ${ENV_VAR} placeholders.
                    let config = mcp::load_mcp_servers_config()?;
                    let server = config.servers.get(&server_name).ok_or_else(|| {
                        anyhow::anyhow!("server '{server_name}' not found in config")
                    })?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(mcp::discover_tools(&server_name, server))
                })
                .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(tools) => DiscoveryState::Loaded(tools),
                    Err(error) => DiscoveryState::Failed(format!("{error:#}")),
                };
                this.discovered.insert(name, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_add_form(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.form_mode = FormMode::Adding;
        self.fill_form("", None, window, cx);
        cx.notify();
    }

    fn open_edit_form(&mut self, name: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let server = self.config.servers.get(name).cloned();
        self.form_mode = FormMode::Editing(name.to_string());
        self.fill_form(name, server.as_ref(), window, cx);
        cx.notify();
    }

    fn fill_form(
        &mut self,
        name: &str,
        server: Option<&McpServerConfig>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let command = server.map(|s| s.command.clone()).unwrap_or_default();
        let args = server.map(|s| s.args.join(" ")).unwrap_or_default();
        let env = server
            .map(|s| {
                let mut entries: Vec<_> = s.env.iter().collect();
                entries.sort();
                entries
                    .into_iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
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
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let name = self.form_name_input.read(cx).value().trim().to_string();
        let command = self.form_command_input.read(cx).value().trim().to_string();
        if name.is_empty() || command.is_empty() {
            warn!("MCP server needs both a name and a command");
            return;
        }
        let args: Vec<String> = self
            .form_args_input
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let env: HashMap<String, String> = self
            .form_env_input
            .read(cx)
            .value()
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                let (key, value) = line.split_once('=')?;
                Some((key.trim().to_string(), value.trim().to_string()))
            })
            .collect();

        // Renaming moves the entry (and its tool filter) to the new key.
        let previous = match &self.form_mode {
            FormMode::Editing(old_name) => self.config.servers.remove(old_name),
            _ => None,
        };
        let mut server = previous.unwrap_or_else(|| McpServerConfig {
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            enabled_tools: None,
            disabled_tools: Vec::new(),
        });
        server.command = command;
        server.args = args;
        server.env = env;
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

        let summary = if server.args.is_empty() {
            server.command.clone()
        } else {
            format!("{} {}", server.command, server.args.join(" "))
        };

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
                                    .font_weight(gpui::FontWeight::MEDIUM)
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
        let name_for_retry = name.to_string();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_3()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            // Action row
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
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
                                    .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                                    .hover(|s| s.bg(gpui::hsla(0.0, 0.7, 0.5, 0.1)))
                                    .child("Delete")
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.delete_server(&name_for_delete, cx);
                                    })),
                            ),
                    ),
            )
            // Tool list by discovery state
            .child(match self.discovered.get(name) {
                None | Some(DiscoveryState::Loading) => div()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Connecting to server…")
                    .into_any_element(),
                Some(DiscoveryState::Failed(error)) => div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
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
            })
    }

    fn render_tool_row(
        &self,
        server_name: &str,
        server: &McpServerConfig,
        tool: &DiscoveredTool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let enabled = server.is_tool_enabled(&tool.name);
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
            .child(
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
    }

    /// The add/edit form: name, command, args, env.
    fn render_inline_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing_name = match &self.form_mode {
            FormMode::Editing(name) => Some(name.clone()),
            _ => None,
        };

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
            .child(self.render_form_row(
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
                Input::new(&self.form_env_input).into_any_element(),
                cx,
            ))
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
                                .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                                .hover(|s| s.bg(gpui::hsla(0.0, 0.7, 0.5, 0.1)))
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

    fn render_form_row(
        &self,
        label: &str,
        widget: gpui::AnyElement,
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
                    .font_weight(gpui::FontWeight::MEDIUM)
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
                gpui::MouseButton::Left,
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
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div().px_4().py_3().child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("New MCP Server"),
                        ),
                    )
                    .child(self.render_inline_form(cx)),
            )
    }
}

fn load_config() -> McpServersConfig {
    mcp::load_mcp_servers_config_raw().unwrap_or_else(|e| {
        warn!("Failed to load mcp-servers.json: {e:#}");
        McpServersConfig::default()
    })
}

impl Focusable for McpSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for McpSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
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
            // Add dialog overlay
            .when(self.form_mode == FormMode::Adding, |el| {
                el.child(self.render_add_dialog(cx))
            })
    }
}
