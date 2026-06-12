//! Models settings section — list configured models, add/edit/remove.

use gpui::{div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, SharedString};
use gpui_component::input::{Input, InputState};
use gpui_component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_component::{ActiveTheme, Icon, Sizable, Size};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use tracing::{debug, warn};

/// A single model entry from models.json.
#[derive(Clone, Debug)]
pub struct ModelEntry {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub context_limit: u32,
}

/// A provider reference for the dropdown.
#[derive(Clone, Debug)]
struct ProviderItem {
    key: String,
    label: String,
}

impl SelectItem for ProviderItem {
    type Value = String;

    fn title(&self) -> SharedString {
        SharedString::from(self.label.clone())
    }

    fn value(&self) -> &Self::Value {
        &self.key
    }
}

/// State of the model form.
#[derive(Clone, Debug, PartialEq)]
enum FormMode {
    Hidden,
    Adding,
    Editing(String),
}

pub struct ModelsSection {
    focus_handle: FocusHandle,
    models: Vec<ModelEntry>,
    providers: Vec<ProviderItem>,
    form_mode: FormMode,
    // Form inputs
    form_name_input: Entity<InputState>,
    form_model_id_input: Entity<InputState>,
    form_context_limit_input: Entity<InputState>,
    form_provider_select: Entity<SelectState<Vec<ProviderItem>>>,
    _provider_select_subscription: gpui::Subscription,
    form_selected_provider: Option<String>,
}

impl ModelsSection {
    pub fn new(window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let models = Self::load_models();
        let providers = Self::load_providers();

        let form_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Claude Sonnet"));
        let form_model_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. claude-sonnet-4-20250514"));
        let form_context_limit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 200000"));

        let provider_items = providers.clone();
        let form_provider_select =
            cx.new(|cx| SelectState::new(Vec::<ProviderItem>::new(), None, window, cx));
        form_provider_select.update(cx, |state, cx| {
            state.set_items(provider_items, window, cx);
        });

        let provider_select_subscription =
            cx.subscribe_in(&form_provider_select, window, Self::on_provider_selected);

        Self {
            focus_handle: cx.focus_handle(),
            models,
            providers,
            form_mode: FormMode::Hidden,
            form_name_input,
            form_model_id_input,
            form_context_limit_input,
            form_provider_select,
            _provider_select_subscription: provider_select_subscription,
            form_selected_provider: None,
        }
    }

    fn on_provider_selected(
        &mut self,
        _: &Entity<SelectState<Vec<ProviderItem>>>,
        event: &SelectEvent<Vec<ProviderItem>>,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(provider_key)) = event {
            self.form_selected_provider = Some(provider_key.clone());
            cx.notify();
        }
    }

    /// Reload models from disk.
    pub fn reload(&mut self) {
        self.models = Self::load_models();
        self.providers = Self::load_providers();
    }

    fn load_models() -> Vec<ModelEntry> {
        let config_path = llm::provider_config::ConfigurationSystem::models_config_path();
        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let map: BTreeMap<String, serde_json::Value> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse models.json: {}", e);
                return Vec::new();
            }
        };

        map.into_iter()
            .map(|(name, value)| {
                let provider = value
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let model_id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let context_limit = value
                    .get("context_token_limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                ModelEntry {
                    name,
                    provider,
                    model_id,
                    context_limit,
                }
            })
            .collect()
    }

    fn load_providers() -> Vec<ProviderItem> {
        let config_path = llm::provider_config::ConfigurationSystem::providers_config_path();
        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let map: BTreeMap<String, serde_json::Value> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse providers.json: {}", e);
                return Vec::new();
            }
        };

        map.into_iter()
            .map(|(key, value)| {
                let label = value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&key)
                    .to_string();
                ProviderItem { key, label }
            })
            .collect()
    }

    fn provider_exists(&self, provider_key: &str) -> bool {
        self.providers.iter().any(|p| p.key == provider_key)
    }

    fn provider_label(&self, provider_key: &str) -> Option<&str> {
        self.providers
            .iter()
            .find(|p| p.key == provider_key)
            .map(|p| p.label.as_str())
    }

    fn render_model_card(&self, entry: &ModelEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let context_str = if entry.context_limit >= 1000 {
            format!("{}K context", entry.context_limit / 1000)
        } else if entry.context_limit > 0 {
            format!("{} tokens", entry.context_limit)
        } else {
            "no limit set".to_string()
        };

        let provider_missing = !self.provider_exists(&entry.provider);
        let is_expanded = self.form_mode == FormMode::Editing(entry.name.clone());
        let name_for_click = entry.name.clone();

        let provider_display = if provider_missing {
            format!("{} (not configured)", entry.provider)
        } else {
            self.provider_label(&entry.provider)
                .unwrap_or(&entry.provider)
                .to_string()
        };

        div()
            .id(SharedString::from(format!("model-{}", entry.name)))
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(if is_expanded {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .bg(cx.theme().secondary)
            .overflow_hidden()
            // Card header (clickable)
            .child(
                div()
                    .id(SharedString::from(format!("model-header-{}", entry.name)))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.form_mode == FormMode::Editing(name_for_click.clone()) {
                            this.form_mode = FormMode::Hidden;
                        } else {
                            this.populate_form_from_entry(&name_for_click, window, cx);
                            this.form_mode = FormMode::Editing(name_for_click.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(entry.name.clone())),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if provider_missing {
                                                gpui::hsla(0.0, 0.7, 0.5, 1.0)
                                            } else {
                                                cx.theme().muted_foreground
                                            })
                                            .child(SharedString::from(provider_display)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from(format!(
                                                "· {}",
                                                context_str
                                            ))),
                                    ),
                            )
                            .when(!entry.model_id.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                                        .child(SharedString::from(format!(
                                            "ID: {}",
                                            entry.model_id
                                        ))),
                                )
                            }),
                    )
                    // Provider status indicator
                    .child(
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .rounded_full()
                            .bg(if provider_missing {
                                gpui::hsla(0.0, 0.7, 0.5, 1.0)
                            } else {
                                gpui::hsla(0.33, 0.7, 0.45, 1.0)
                            }),
                    )
                    // Expand chevron
                    .child(
                        Icon::default()
                            .path(SharedString::from(if is_expanded {
                                "icons/chevron_up.svg"
                            } else {
                                "icons/chevron_down.svg"
                            }))
                            .with_size(Size::XSmall)
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            // Inline form when expanded
            .when(is_expanded, |el| el.child(self.render_model_form(cx)))
    }

    fn populate_form_from_entry(
        &mut self,
        name: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self.models.iter().find(|e| e.name == name).cloned();
        if let Some(entry) = entry {
            self.form_name_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(entry.name), window, cx);
            });
            self.form_model_id_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(entry.model_id), window, cx);
            });
            self.form_context_limit_input.update(cx, |state, cx| {
                state.set_value(
                    SharedString::from(entry.context_limit.to_string()),
                    window,
                    cx,
                );
            });
            self.form_selected_provider = Some(entry.provider.clone());
            self.form_provider_select.update(cx, |state, cx| {
                state.set_selected_value(&entry.provider, window, cx);
            });
        }
    }

    fn reset_form(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.form_name_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.form_model_id_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.form_context_limit_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.form_selected_provider = None;
        // Refresh provider list in dropdown
        let providers = Self::load_providers();
        self.form_provider_select.update(cx, |state, cx| {
            state.set_items(providers.clone(), window, cx);
        });
        self.providers = providers;
    }

    fn render_model_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing_key = match &self.form_mode {
            FormMode::Editing(key) => Some(key.clone()),
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
            // Display Name
            .child(self.render_form_row(
                "Name",
                Input::new(&self.form_name_input).into_any_element(),
                cx,
            ))
            // Provider dropdown
            .child(
                self.render_form_row(
                    "Provider",
                    div()
                        .child(
                            Select::new(&self.form_provider_select)
                                .placeholder("Select provider")
                                .with_size(Size::Small)
                                .icon(
                                    Icon::default()
                                        .path("icons/chevron_up_down.svg")
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .min_w(px(200.)),
                        )
                        .into_any_element(),
                    cx,
                ),
            )
            // Model ID
            .child(self.render_form_row(
                "Model ID",
                Input::new(&self.form_model_id_input).into_any_element(),
                cx,
            ))
            // Context limit
            .child(self.render_form_row(
                "Context",
                Input::new(&self.form_context_limit_input).into_any_element(),
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
                    // Left: Delete button (only when editing)
                    .child(div().when_some(editing_key.clone(), |el, key| {
                        el.child(
                            div()
                                .id("delete-model")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                                .hover(|s| s.bg(gpui::hsla(0.0, 0.7, 0.5, 0.1)))
                                .child("Delete")
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.delete_model(&key, cx);
                                })),
                        )
                    }))
                    // Right: Cancel + Save
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("cancel-model-form")
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
                                    .id("save-model")
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
                                        this.save_model(cx);
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

    fn save_model(&mut self, cx: &mut Context<Self>) {
        let name = self.form_name_input.read(cx).value().to_string();
        let model_id = self.form_model_id_input.read(cx).value().to_string();
        let context_str = self.form_context_limit_input.read(cx).value().to_string();

        if name.trim().is_empty() || model_id.trim().is_empty() {
            return;
        }

        let provider = match &self.form_selected_provider {
            Some(p) => p.clone(),
            None => return,
        };

        let context_limit: u32 = context_str.trim().parse().unwrap_or(200_000);

        // Load existing models
        let config_path = llm::provider_config::ConfigurationSystem::models_config_path();
        let mut map: Map<String, Value> = match std::fs::read_to_string(&config_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Map::new(),
        };

        // When editing, remove old key if name changed
        if let FormMode::Editing(ref old_name) = self.form_mode {
            if old_name != name.trim() {
                map.remove(old_name);
            }
        }

        // Build model entry
        let model_value = serde_json::json!({
            "provider": provider,
            "id": model_id.trim(),
            "config": {},
            "context_token_limit": context_limit,
        });

        map.insert(name.trim().to_string(), model_value);

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&config_path, json) {
                    warn!("Failed to write models.json: {}", e);
                } else {
                    debug!("Saved model to {}", config_path.display());
                    self.form_mode = FormMode::Hidden;
                    self.reload();
                }
            }
            Err(e) => {
                warn!("Failed to serialize models: {}", e);
            }
        }

        cx.notify();
    }

    fn delete_model(&mut self, name: &str, cx: &mut Context<Self>) {
        let config_path = llm::provider_config::ConfigurationSystem::models_config_path();
        let mut map: Map<String, Value> = match std::fs::read_to_string(&config_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => return,
        };

        map.remove(name);

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&config_path, json) {
                    warn!("Failed to write models.json: {}", e);
                } else {
                    debug!("Deleted model '{}' from {}", name, config_path.display());
                    self.form_mode = FormMode::Hidden;
                    self.reload();
                }
            }
            Err(e) => {
                warn!("Failed to serialize models: {}", e);
            }
        }

        cx.notify();
    }

    fn render_add_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-model-btn")
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
                this.form_mode = FormMode::Adding;
                this.reset_form(window, cx);
                cx.notify();
            }))
    }

    fn render_add_model_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-model-dialog-backdrop")
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
                    .id("add-model-dialog")
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
                    // Title
                    .child(
                        div().px_4().py_3().child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("New Model"),
                        ),
                    )
                    // Form content
                    .child(self.render_model_form(cx)),
            )
    }
}

impl Focusable for ModelsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelsSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form_mode = self.form_mode.clone();
        let has_providers = !self.providers.is_empty();

        div()
            .relative()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(700.))
                    .mx_auto()
                    // Header
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
                                    .child("MODELS"),
                            )
                            .when(has_providers, |el| el.child(self.render_add_button(cx))),
                    )
                    // No providers warning
                    .when(!has_providers, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .py_8()
                                .gap_3()
                                .child(
                                    div()
                                        .text_base()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Please configure providers first"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Models need a provider. Go to the Providers section to add one.",
                                        ),
                                ),
                        )
                    })
                    // Model cards
                    .when(has_providers, |el| {
                        el.children(
                            self.models
                                .clone()
                                .iter()
                                .map(|entry| {
                                    self.render_model_card(entry, cx).into_any_element()
                                }),
                        )
                    })
                    // Empty state (providers exist but no models)
                    .when(has_providers && self.models.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .py_8()
                                .gap_3()
                                .child(
                                    div()
                                        .text_base()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No models configured yet"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Click \"+ Add\" above to configure a model.",
                                        ),
                                ),
                        )
                    }),
            )
            // Dialog overlay for adding
            .when(form_mode == FormMode::Adding, |el| {
                el.child(self.render_add_model_dialog(cx))
            })
    }
}
