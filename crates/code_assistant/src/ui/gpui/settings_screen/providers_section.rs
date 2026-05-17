//! Providers settings section — list configured providers, add/edit/remove.

use super::provider_forms::ProviderFormHolder;
use gpui::{div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, SharedString};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Icon, Sizable, Size};
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

pub struct ProvidersSection {
    focus_handle: FocusHandle,
    providers: Vec<ProviderEntry>,
    form_mode: FormMode,
    // Input states for the add/edit form (label is always present)
    form_label_input: Entity<InputState>,
    form_provider_type: String,
    // Provider-specific form
    form_holder: ProviderFormHolder,
}

impl ProvidersSection {
    pub fn new(window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let providers = Self::load_providers();

        let form_label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. My Anthropic"));

        let form_holder = ProviderFormHolder::new("anthropic", window, cx);

        Self {
            focus_handle: cx.focus_handle(),
            providers,
            form_mode: FormMode::Hidden,
            form_label_input,
            form_provider_type: "anthropic".to_string(),
            form_holder,
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
                        // For AI Core, show api_base_url instead
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

    /// Reload providers from disk.
    fn reload(&mut self) {
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

        // Switch form holder to correct provider type
        self.form_holder.switch_to(&entry.provider_type, window, cx);

        // Populate provider-specific fields from raw config
        if let Some(ref config) = entry.raw_config {
            self.form_holder.load_config(config, window, cx);
        }
    }

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
                            // Collapse
                            this.form_mode = FormMode::Hidden;
                        } else {
                            // Expand — populate form with current values
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
                this.form_holder.switch_to("anthropic", window, cx);
                this.form_holder.reset(window, cx);
                // Reset label
                this.form_label_input.update(cx, |state, cx| {
                    state.set_value(SharedString::from(""), window, cx);
                });
                cx.notify();
            }))
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_editing = matches!(&self.form_mode, FormMode::Editing(_));
        let editing_key = match &self.form_mode {
            FormMode::Editing(key) => Some(key.clone()),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().primary)
            .bg(cx.theme().secondary)
            // Title
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(cx.theme().foreground)
                    .child(if is_editing {
                        "Edit Provider"
                    } else {
                        "New Provider"
                    }),
            )
            // Label field (always present)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Label"),
                    )
                    .child(Input::new(&self.form_label_input)),
            )
            // Provider type selector
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Provider Type"),
                    )
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            [
                                ("anthropic", "Anthropic"),
                                ("openai", "OpenAI"),
                                ("ollama", "Ollama"),
                                ("openrouter", "OpenRouter"),
                                ("vertex", "Google"),
                                ("cerebras", "Cerebras"),
                                ("groq", "Groq"),
                                ("mistral-ai", "Mistral"),
                                ("ai-core", "SAP AI Core"),
                            ]
                            .into_iter()
                            .map(|(type_id, display_name)| {
                                let is_selected = self.form_provider_type == type_id;
                                let type_id_owned = type_id.to_string();
                                div()
                                    .id(SharedString::from(format!("ptype-{}", type_id)))
                                    .px_2()
                                    .py(px(3.))
                                    .rounded_md()
                                    .border_1()
                                    .cursor_pointer()
                                    .text_xs()
                                    .when(is_selected, |s| {
                                        s.border_color(cx.theme().primary)
                                            .bg(cx.theme().primary.opacity(0.1))
                                            .text_color(cx.theme().primary)
                                    })
                                    .when(!is_selected, |s| {
                                        s.border_color(cx.theme().border)
                                            .text_color(cx.theme().muted_foreground)
                                            .hover(|s| s.border_color(cx.theme().ring))
                                    })
                                    .child(SharedString::from(display_name.to_string()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.form_provider_type = type_id_owned.clone();
                                        this.form_holder.switch_to(&type_id_owned, window, cx);
                                        cx.notify();
                                    }))
                            }),
                        ),
                    ),
            )
            // Provider-specific form fields (delegated to form holder)
            .child(self.form_holder.render())
            // Action buttons
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt_1()
                    // Left: Delete button (only when editing)
                    .child(div().when_some(editing_key.clone(), |el, key| {
                        el.child(
                            div()
                                .id("delete-provider")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_sm()
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
                                    .text_sm()
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
                                    .text_sm()
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
                    // For the default form: preserve api_key if not provided
                    if !config.contains_key("api_key") {
                        if let Some(existing_key_val) = existing_config.get("api_key") {
                            config.insert("api_key".to_string(), existing_key_val.clone());
                        }
                    }
                    // For AI Core: preserve client_secret if not provided
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
            // Provider cards with inline edit form
            .children(self.providers.clone().iter().map(|entry| {
                let is_editing_this = form_mode == FormMode::Editing(entry.key.clone());
                div()
                    .flex()
                    .flex_col()
                    .child(self.render_provider_card(entry, cx))
                    .when(is_editing_this, |el| el.child(self.render_form(cx)))
                    .into_any_element()
            }))
            // Add form (when adding new)
            .when(form_mode == FormMode::Adding, |el| {
                el.child(self.render_form(cx))
            })
            // Empty state
            .when(
                self.providers.is_empty() && form_mode == FormMode::Hidden,
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
