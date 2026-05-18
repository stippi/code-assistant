//! Providers settings section — list configured providers, add/edit/remove.

use super::provider_forms::ProviderFormHolder;
use super::provider_suggestions::{self, ProviderSuggestion, UserEnvironment};
use gpui::{div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, SharedString};
use gpui_component::input::{Input, InputState};
use gpui_component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_component::{ActiveTheme, Icon, Sizable, Size};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use tracing::{debug, warn};

/// A single provider entry loaded from providers.json.
#[derive(Clone, Debug)]
pub struct ProviderEntry {
    pub key: String,
    pub label: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub has_api_key: bool,
    /// The raw config object from providers.json (for populating provider-specific forms)
    pub raw_config: Option<serde_json::Map<String, serde_json::Value>>,
}

/// State of the "Add Provider" form.
#[derive(Clone, Debug, PartialEq)]
enum FormMode {
    /// No form visible.
    Hidden,
    /// Adding a new provider.
    Adding,
    /// Editing an existing provider (by key).
    Editing(String),
}

/// Item for the provider type dropdown.
#[derive(Clone, Debug)]
struct ProviderTypeItem {
    id: String,
    display_name: String,
}

impl SelectItem for ProviderTypeItem {
    type Value = String;

    fn title(&self) -> SharedString {
        SharedString::from(self.display_name.clone())
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

const PROVIDER_TYPES: &[(&str, &str)] = &[
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("openai-responses", "OpenAI Responses"),
    ("openai-responses-ws", "ChatGPT Subscription"),
    ("ollama", "Ollama"),
    ("openrouter", "OpenRouter"),
    ("vertex", "Google Vertex"),
    ("cerebras", "Cerebras"),
    ("groq", "Groq"),
    ("mistral-ai", "Mistral"),
    ("ai-core", "SAP AI Core"),
];

fn provider_type_items() -> Vec<ProviderTypeItem> {
    PROVIDER_TYPES
        .iter()
        .map(|(id, name)| ProviderTypeItem {
            id: id.to_string(),
            display_name: name.to_string(),
        })
        .collect()
}

pub struct ProvidersSection {
    focus_handle: FocusHandle,
    providers: Vec<ProviderEntry>,
    form_mode: FormMode,
    // Input states for the add/edit form (label is always present)
    form_label_input: Entity<InputState>,
    form_provider_type: String,
    // Provider type dropdown
    provider_type_select: Entity<SelectState<Vec<ProviderTypeItem>>>,
    _provider_type_subscription: gpui::Subscription,
    // Provider-specific form
    form_holder: ProviderFormHolder,
    // Onboarding suggestions
    suggestions: Vec<ProviderSuggestion>,
    /// Index of the suggestion currently being configured (expanded with input fields).
    active_suggestion: Option<usize>,
    /// Input states for the active suggestion's required fields.
    suggestion_field_inputs: Vec<Entity<InputState>>,
}

impl ProvidersSection {
    pub fn new(window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let providers = Self::load_providers();

        let form_label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. My Anthropic"));

        // Create provider type dropdown
        let provider_type_select =
            cx.new(|cx| SelectState::new(Vec::<ProviderTypeItem>::new(), None, window, cx));
        provider_type_select.update(cx, |state, cx| {
            state.set_items(provider_type_items(), window, cx);
            state.set_selected_value(&"anthropic".to_string(), window, cx);
        });
        let provider_type_subscription = cx.subscribe_in(
            &provider_type_select,
            window,
            Self::on_provider_type_changed,
        );

        let form_holder = ProviderFormHolder::new("anthropic", window, cx);

        // Detect user environment and get applicable suggestions
        let user_env = UserEnvironment::detect();
        let suggestions = if providers.is_empty() {
            provider_suggestions::get_suggestions(&user_env)
        } else {
            Vec::new()
        };

        Self {
            focus_handle: cx.focus_handle(),
            providers,
            form_mode: FormMode::Hidden,
            form_label_input,
            form_provider_type: "anthropic".to_string(),
            provider_type_select,
            _provider_type_subscription: provider_type_subscription,
            form_holder,
            suggestions,
            active_suggestion: None,
            suggestion_field_inputs: Vec::new(),
        }
    }

    fn on_provider_type_changed(
        &mut self,
        _: &Entity<SelectState<Vec<ProviderTypeItem>>>,
        event: &SelectEvent<Vec<ProviderTypeItem>>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(provider_id)) = event {
            self.form_provider_type = provider_id.clone();
            self.form_holder.switch_to(provider_id, window, cx);
            cx.notify();
        }
    }

    /// Load providers from the configuration file.
    fn load_providers() -> Vec<ProviderEntry> {
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
                let provider_type = value
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let base_url = value
                    .get("config")
                    .and_then(|c| c.get("base_url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        value
                            .get("config")
                            .and_then(|c| c.get("api_base_url"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                let has_api_key = value
                    .get("config")
                    .and_then(|c| c.get("api_key").or_else(|| c.get("client_id")))
                    .is_some();
                let raw_config = value.get("config").and_then(|c| c.as_object()).cloned();

                ProviderEntry {
                    key,
                    label,
                    provider_type,
                    base_url,
                    has_api_key,
                    raw_config,
                }
            })
            .collect()
    }

    /// Reload providers from disk (called when this section becomes visible).
    pub fn reload(&mut self) {
        self.providers = Self::load_providers();
    }

    /// Get the icon path for a provider type.
    fn provider_icon(provider_type: &str) -> &'static str {
        match provider_type {
            "anthropic" => "icons/ai_anthropic.svg",
            "openai" | "openai-responses" | "openai-responses-ws" => "icons/ai_open_ai.svg",
            "ollama" => "icons/ai_ollama.svg",
            "vertex" => "icons/ai_google.svg",
            "openrouter" => "icons/ai_open_router.svg",
            "cerebras" => "icons/ai_cerebras.svg",
            "groq" => "icons/ai_groq.svg",
            "mistral-ai" => "icons/ai_mistral.svg",
            "ai-core" => "icons/ai_sap.svg",
            _ => "icons/braces.svg",
        }
    }

    /// Populate form fields from an existing provider entry for editing.
    fn populate_form_from_entry(
        &mut self,
        entry: &ProviderEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Set label
        self.form_label_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(entry.label.clone()), window, cx);
        });

        // Set provider type dropdown
        self.provider_type_select.update(cx, |state, cx| {
            state.set_selected_value(&entry.provider_type, window, cx);
        });

        // Switch form holder to correct provider type
        self.form_holder.switch_to(&entry.provider_type, window, cx);

        // Populate provider-specific fields from raw config
        if let Some(ref config) = entry.raw_config {
            self.form_holder.load_config(config, window, cx);
        }
    }

    /// Render a single provider card. When expanded, the form is inside the card.
    fn render_provider_card(
        &self,
        entry: &ProviderEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_expanded = self.form_mode == FormMode::Editing(entry.key.clone());

        let subtitle = if let Some(url) = &entry.base_url {
            format!("{} · {}", entry.provider_type, url)
        } else {
            entry.provider_type.clone()
        };

        let icon_path = Self::provider_icon(&entry.provider_type);
        let key_for_click = entry.key.clone();
        let header_id = SharedString::from(format!("provider-header-{}", entry.key));

        div()
            .id(SharedString::from(format!("provider-{}", entry.key)))
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
            // Header row (always visible)
            .child(
                div()
                    .id(header_id)
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.form_mode == FormMode::Editing(key_for_click.clone()) {
                            this.form_mode = FormMode::Hidden;
                        } else {
                            if let Some(entry) =
                                this.providers.iter().find(|e| e.key == key_for_click)
                            {
                                let entry = entry.clone();
                                this.form_provider_type = entry.provider_type.clone();
                                this.populate_form_from_entry(&entry, window, cx);
                            }
                            this.form_mode = FormMode::Editing(key_for_click.clone());
                        }
                        cx.notify();
                    }))
                    // Provider icon
                    .child(
                        div().flex_none().child(
                            Icon::default()
                                .path(SharedString::from(icon_path.to_string()))
                                .with_size(Size::Medium)
                                .text_color(cx.theme().foreground),
                        ),
                    )
                    // Info column
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
                                    .child(SharedString::from(entry.label.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(subtitle)),
                            ),
                    )
                    // Status dot + chevron
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(8.)).rounded_full().bg(if entry.has_api_key {
                                gpui::hsla(142.0 / 360.0, 0.7, 0.45, 1.0)
                            } else {
                                gpui::hsla(0.0, 0.0, 0.6, 1.0)
                            }))
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
                    ),
            )
            // Inline form (inside the card when expanded)
            .when(is_expanded, |el| el.child(self.render_inline_form(cx)))
    }

    /// Render the form content that appears inside a provider card or as a standalone new-provider card.
    fn render_inline_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            // Form rows (label + widget side by side)
            // Label field
            .child(self.render_form_row(
                "Label",
                Input::new(&self.form_label_input).into_any_element(),
                cx,
            ))
            // Provider type dropdown
            .child(
                self.render_form_row(
                    "Type",
                    div()
                        .child(
                            Select::new(&self.provider_type_select)
                                .placeholder("Select provider type")
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
            // Provider-specific form fields
            .child(self.form_holder.render())
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
                                .id("delete-provider")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                                .hover(|s| s.bg(gpui::hsla(0.0, 0.7, 0.5, 0.1)))
                                .child("Delete")
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.delete_provider(&key, cx);
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
                                    .id("cancel-form")
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
                                    .id("save-provider")
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
                                        this.save_provider(cx);
                                    })),
                            ),
                    ),
            )
    }

    /// Render a form row with label on the left and widget on the right.
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

    fn render_add_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-provider-btn")
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted))
            .child(
                Icon::default()
                    .path(SharedString::from("icons/plus.svg"))
                    .with_size(Size::XSmall)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Add"),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.form_mode = FormMode::Adding;
                this.form_provider_type = "anthropic".to_string();
                this.provider_type_select.update(cx, |state, cx| {
                    state.set_selected_value(&"anthropic".to_string(), window, cx);
                });
                this.form_holder.switch_to("anthropic", window, cx);
                this.form_holder.reset(window, cx);
                this.form_label_input.update(cx, |state, cx| {
                    state.set_value(SharedString::from(""), window, cx);
                });
                cx.notify();
            }))
    }

    fn save_provider(&mut self, cx: &mut Context<Self>) {
        let label = self.form_label_input.read(cx).value().to_string();

        if label.is_empty() {
            return;
        }

        // Determine the key: use existing key when editing, generate from label when adding
        let key = match &self.form_mode {
            FormMode::Editing(existing_key) => existing_key.clone(),
            _ => label
                .to_lowercase()
                .replace(' ', "-")
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>(),
        };

        // Load existing file
        let config_path = llm::provider_config::ConfigurationSystem::providers_config_path();
        let mut map: serde_json::Map<String, serde_json::Value> =
            match std::fs::read_to_string(&config_path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => serde_json::Map::new(),
            };

        // Get config from the provider-specific form
        let mut config = self.form_holder.to_config_json(cx);

        // When editing, preserve fields that weren't overwritten
        if let FormMode::Editing(ref existing_key) = self.form_mode {
            if let Some(existing) = map.get(existing_key) {
                if let Some(existing_config) = existing.get("config").and_then(|c| c.as_object()) {
                    if !config.contains_key("api_key") {
                        if let Some(existing_key_val) = existing_config.get("api_key") {
                            config.insert("api_key".to_string(), existing_key_val.clone());
                        }
                    }
                    if !config.contains_key("client_secret")
                        || config
                            .get("client_secret")
                            .and_then(|v: &serde_json::Value| v.as_str())
                            .map(|s: &str| s.is_empty())
                            .unwrap_or(false)
                    {
                        if let Some(existing_val) = existing_config.get("client_secret") {
                            config.insert("client_secret".to_string(), existing_val.clone());
                        }
                    }
                }
            }
        }

        // Remove empty string values
        config.retain(|_, v: &mut serde_json::Value| {
            if let Some(s) = v.as_str() {
                !s.is_empty()
            } else {
                true
            }
        });

        // Set default base_url for standard providers if not specified
        if !config.contains_key("base_url") && !config.contains_key("api_base_url") {
            let default_url = match self.form_provider_type.as_str() {
                "anthropic" => Some("https://api.anthropic.com/v1"),
                "openai" | "openai-responses" => Some("https://api.openai.com/v1"),
                "ollama" => Some("http://localhost:11434"),
                "openrouter" => Some("https://openrouter.ai/api/v1"),
                "vertex" => Some("https://generativelanguage.googleapis.com/v1beta"),
                "cerebras" => Some("https://api.cerebras.ai/v1"),
                "groq" => Some("https://api.groq.com/openai/v1"),
                "mistral-ai" => Some("https://api.mistral.ai/v1"),
                _ => None,
            };
            if let Some(url) = default_url {
                config.insert(
                    "base_url".to_string(),
                    serde_json::Value::String(url.to_string()),
                );
            }
        }

        let mut entry = serde_json::Map::new();
        entry.insert("label".to_string(), serde_json::Value::String(label));
        entry.insert(
            "provider".to_string(),
            serde_json::Value::String(self.form_provider_type.clone()),
        );
        entry.insert("config".to_string(), serde_json::Value::Object(config));

        map.insert(key, serde_json::Value::Object(entry));

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&config_path, json) {
                    warn!("Failed to write providers.json: {}", e);
                } else {
                    debug!("Saved provider to {}", config_path.display());
                    self.form_mode = FormMode::Hidden;
                    self.reload();
                }
            }
            Err(e) => {
                warn!("Failed to serialize providers: {}", e);
            }
        }

        cx.notify();
    }

    fn delete_provider(&mut self, key: &str, cx: &mut Context<Self>) {
        let config_path = llm::provider_config::ConfigurationSystem::providers_config_path();
        let mut map: serde_json::Map<String, serde_json::Value> =
            match std::fs::read_to_string(&config_path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => return,
            };

        map.remove(key);

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&config_path, json) {
                    warn!("Failed to write providers.json: {}", e);
                } else {
                    debug!("Deleted provider '{}' from {}", key, config_path.display());
                    self.form_mode = FormMode::Hidden;
                    self.reload();
                }
            }
            Err(e) => {
                warn!("Failed to serialize providers: {}", e);
            }
        }

        cx.notify();
    }

    // -------------------------------------------------------------------------
    // Suggestion cards (onboarding)
    // -------------------------------------------------------------------------

    /// Expand a suggestion card: create input states for its required fields.
    fn expand_suggestion(
        &mut self,
        index: usize,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.suggestions.len() {
            return;
        }
        let suggestion = &self.suggestions[index];
        let inputs: Vec<Entity<InputState>> = suggestion
            .required_fields
            .iter()
            .map(|field| cx.new(|cx| InputState::new(window, cx).placeholder(field.placeholder)))
            .collect();

        self.active_suggestion = Some(index);
        self.suggestion_field_inputs = inputs;
        cx.notify();
    }

    /// Apply the active suggestion with user-provided field values.
    fn apply_active_suggestion(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_suggestion else {
            return;
        };
        let suggestion = self.suggestions[index].clone();

        // Collect user-provided fields
        let mut user_fields = Map::new();
        for (i, field) in suggestion.required_fields.iter().enumerate() {
            if let Some(input) = self.suggestion_field_inputs.get(i) {
                let value = input.read(cx).value().to_string();
                if !value.is_empty() {
                    user_fields.insert(field.key.to_string(), Value::String(value));
                }
            }
        }

        // Check required fields are filled
        let all_filled = suggestion.required_fields.iter().enumerate().all(|(i, _)| {
            self.suggestion_field_inputs
                .get(i)
                .map(|input| !input.read(cx).value().is_empty())
                .unwrap_or(false)
        });

        // If no required fields, or all are filled, apply
        if !all_filled && !suggestion.required_fields.is_empty() {
            // TODO: show validation error
            return;
        }

        match provider_suggestions::apply_suggestion(&suggestion, &user_fields) {
            Ok(()) => {
                debug!("Applied suggestion '{}' successfully", suggestion.title);
                // Remove this suggestion and reload providers
                self.suggestions.remove(index);
                self.active_suggestion = None;
                self.suggestion_field_inputs.clear();
                self.reload();
            }
            Err(e) => {
                warn!("Failed to apply suggestion '{}': {}", suggestion.title, e);
            }
        }

        cx.notify();
    }

    /// Render the suggestions section (shown when no providers exist).
    fn render_suggestions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            // Header
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pb_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child("Quick Setup"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Choose a provider to get started quickly, or use \"+ Add\" above for manual configuration."),
                    ),
            )
            // Suggestion cards
            .children(
                self.suggestions
                    .iter()
                    .enumerate()
                    .map(|(i, suggestion)| {
                        self.render_suggestion_card(i, suggestion, cx)
                            .into_any_element()
                    }),
            )
    }

    /// Render a single suggestion card.
    fn render_suggestion_card(
        &self,
        index: usize,
        suggestion: &ProviderSuggestion,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_expanded = self.active_suggestion == Some(index);

        div()
            .id(SharedString::from(suggestion.id.to_string()))
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
            // Header (clickable to expand)
            .child(
                div()
                    .id(SharedString::from(format!("{}-header", suggestion.id)))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.active_suggestion == Some(index) {
                            this.active_suggestion = None;
                            this.suggestion_field_inputs.clear();
                            cx.notify();
                        } else {
                            this.expand_suggestion(index, window, cx);
                        }
                    }))
                    // Icon
                    .child(
                        div().flex_none().child(
                            Icon::default()
                                .path(SharedString::from(suggestion.icon.to_string()))
                                .with_size(Size::Medium)
                                .text_color(cx.theme().foreground),
                        ),
                    )
                    // Info
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
                                    .child(SharedString::from(suggestion.title.to_string())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(suggestion.description.to_string())),
                            ),
                    )
                    // Chevron
                    .child(
                        div().flex_none().child(
                            Icon::default()
                                .path(SharedString::from(if is_expanded {
                                    "icons/chevron_up.svg"
                                } else {
                                    "icons/chevron_down.svg"
                                }))
                                .with_size(Size::XSmall)
                                .text_color(cx.theme().muted_foreground),
                        ),
                    ),
            )
            // Expanded form (fields + apply button)
            .when(is_expanded, |el| {
                el.child(self.render_suggestion_form(index, suggestion, cx))
            })
    }

    /// Render the expanded form area for a suggestion.
    fn render_suggestion_form(
        &self,
        _index: usize,
        suggestion: &ProviderSuggestion,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_fields = !suggestion.required_fields.is_empty();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            // Required fields
            .when(has_fields, |el| {
                let mut container = el;
                for (i, field) in suggestion.required_fields.iter().enumerate() {
                    if let Some(input_state) = self.suggestion_field_inputs.get(i) {
                        container = container.child(
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
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
                                                .child(SharedString::from(field.label.to_string())),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(200.))
                                                .child(Input::new(input_state)),
                                        ),
                                )
                                .when_some(field.help_text, |el, help| {
                                    el.child(
                                        div()
                                            .pl(px(83.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground.opacity(0.8))
                                            .child(SharedString::from(help.to_string())),
                                    )
                                }),
                        );
                    }
                }
                container
            })
            // Info about what will be created
            .child(
                div().flex().flex_col().gap_1().mt_1().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!(
                            "This will create the provider \"{}\" and {} model{}.",
                            suggestion.provider_config.label,
                            suggestion.models.len(),
                            if suggestion.models.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ))),
                ),
            )
            // Apply button
            .child(
                div()
                    .flex()
                    .justify_end()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("apply-suggestion")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .hover(|s| s.opacity(0.9))
                            .child(if has_fields { "Apply" } else { "Set Up" })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.apply_active_suggestion(cx);
                            })),
                    ),
            )
    }
}

impl Focusable for ProvidersSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProvidersSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form_mode = self.form_mode.clone();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .max_w(px(700.))
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
                            .child("PROVIDERS"),
                    )
                    .child(self.render_add_button(cx)),
            )
            // Provider cards (form is inline inside the expanded card)
            .children(
                self.providers
                    .clone()
                    .iter()
                    .map(|entry| self.render_provider_card(entry, cx).into_any_element()),
            )
            // Add new provider card (standalone form when adding)
            .when(form_mode == FormMode::Adding, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().primary)
                        .bg(cx.theme().secondary)
                        .overflow_hidden()
                        // Title header
                        .child(
                            div().px_4().py_3().child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child("New Provider"),
                            ),
                        )
                        // Form content
                        .child(self.render_inline_form(cx)),
                )
            })
            // Suggestions (shown as long as there are unapplied suggestions, regardless of form state)
            .when(!self.suggestions.is_empty(), |el| {
                el.child(self.render_suggestions(cx))
            })
            // Empty state (only shown if no providers AND no suggestions)
            .when(
                self.providers.is_empty()
                    && self.suggestions.is_empty()
                    && form_mode == FormMode::Hidden,
                |el| {
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
                                    .child("No providers configured yet"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Click \"+ Add\" above to get started."),
                            ),
                    )
                },
            )
    }
}
