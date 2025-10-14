use gpui::{div, prelude::*, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    dropdown::{Dropdown, DropdownEvent, DropdownItem, DropdownState},
    Icon, Sizable, Size,
};
use llm::provider_config::ConfigurationSystem;
use std::sync::Arc;
use tracing::{debug, warn};

/// Events emitted by the ModelSelector component
#[derive(Clone, Debug)]
pub enum ModelSelectorEvent {
    /// Model selection changed
    ModelChanged { model_name: String },
}

/// Model item for the dropdown
#[derive(Clone, Debug)]
pub struct ModelItem {
    name: String,
    provider_label: String,
    icon_path: Option<String>,
}

impl ModelItem {
    pub fn new(name: String, provider_label: String, icon_path: Option<String>) -> Self {
        Self {
            name,
            provider_label,
            icon_path,
        }
    }
}

impl DropdownItem for ModelItem {
    type Value = String;

    fn title(&self) -> gpui::SharedString {
        self.name.clone().into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        let mut row = div().flex().items_center().gap_2().min_w_0();

        if let Some(icon_path) = &self.icon_path {
            row = row.child(
                Icon::default()
                    .path(icon_path.clone())
                    .with_size(Size::Small),
            );
        }

        row = row.child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(div().min_w_0().text_ellipsis().child(self.name.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_ellipsis()
                        .child(format!("• {}", self.provider_label)),
                ),
        );

        Some(row.into_any_element())
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }
}

/// Model selector dropdown component using gpui-component's Dropdown
pub struct ModelSelector {
    dropdown_state: Entity<DropdownState<Vec<ModelItem>>>,
    config: Option<Arc<ConfigurationSystem>>,
    _dropdown_subscription: gpui::Subscription,
}

impl EventEmitter<ModelSelectorEvent> for ModelSelector {}

impl ModelSelector {
    /// Create a new model selector
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dropdown_state =
            cx.new(|cx| DropdownState::new(Vec::<ModelItem>::new(), None, window, cx));

        // Subscribe to dropdown events once during construction
        let dropdown_subscription =
            cx.subscribe_in(&dropdown_state, window, Self::on_dropdown_event);

        let mut selector = Self {
            dropdown_state,
            config: None,
            _dropdown_subscription: dropdown_subscription,
        };

        selector.refresh_models(window, cx);

        selector
    }

    /// Set the current model
    pub fn set_current_model(
        &mut self,
        model_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(model_name) = model_name {
            self.dropdown_state.update(cx, |state, cx| {
                state.set_selected_value(&model_name, window, cx);
            });
        }
    }

    /// Get the current model
    pub fn current_model(&self, cx: &gpui::App) -> Option<String> {
        self.dropdown_state.read(cx).selected_value().cloned()
    }

    /// Handle dropdown events
    fn on_dropdown_event(
        &mut self,
        _: &Entity<DropdownState<Vec<ModelItem>>>,
        event: &DropdownEvent<Vec<ModelItem>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DropdownEvent::Confirm(Some(model_name)) => {
                debug!("Model selected: {}", model_name);
                cx.emit(ModelSelectorEvent::ModelChanged {
                    model_name: model_name.clone(),
                });
            }
            DropdownEvent::Confirm(None) => {
                debug!("Model selection cleared");
                // Optionally handle clearing selection
            }
        }
    }

    /// Refresh the available models (useful when configuration changes)
    pub fn refresh_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = match ConfigurationSystem::load() {
            Ok(config) => Some(Arc::new(config)),
            Err(err) => {
                warn!(
                    error = ?err,
                    "Failed to load configuration system for model selector refresh"
                );
                None
            }
        };

        let model_items = if let Some(ref config) = config {
            let mut items: Vec<ModelItem> = config
                .models
                .iter()
                .filter_map(|(model_name, model_config)| {
                    let provider_config = config.providers.get(&model_config.provider)?;
                    let icon_path =
                        provider_icon_path(&provider_config.provider).map(|path| path.to_string());
                    Some(ModelItem::new(
                        model_name.clone(),
                        provider_config.label.clone(),
                        icon_path,
                    ))
                })
                .collect();

            items.sort_by(|a, b| {
                a.provider_label
                    .cmp(&b.provider_label)
                    .then_with(|| a.name.cmp(&b.name))
            });

            items
        } else {
            Vec::new()
        };

        self.dropdown_state.update(cx, |state, cx| {
            state.set_items(model_items, window, cx);
        });

        self.config = config;
    }
}

fn provider_icon_path(provider_type: &str) -> Option<&'static str> {
    match provider_type {
        "openai" | "openai-responses" => Some("icons/ai_open_ai.svg"),
        "anthropic" => Some("icons/ai_anthropic.svg"),
        "vertex" => Some("icons/ai_google.svg"),
        "mistral-ai" => Some("icons/ai_mistral.svg"),
        "openrouter" => Some("icons/ai_open_router.svg"),
        "ollama" => Some("icons/ai_ollama.svg"),
        "ai-core" => Some("icons/brain.svg"),
        "cerebras" => Some("icons/brain.svg"),
        "groq" => Some("icons/brain.svg"),
        _ => None,
    }
}

impl Focusable for ModelSelector {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for ModelSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div().text_size(gpui::px(12.)).child(
            Dropdown::new(&self.dropdown_state)
                .placeholder("Select Model")
                .with_size(Size::Small)
                .cleanable(),
        )
    }
}

#[cfg(test)]
mod tests {
    // use super::*;

    // Note: These tests would need a proper GPUI test environment
    // For now, they serve as documentation of expected behavior

    #[test]
    fn test_model_selector_creation() {
        // This test would need a mock GPUI context
        // let cx = &mut test_context();
        // let selector = ModelSelector::new(cx);
        // assert_eq!(selector.current_model(), None);
        // assert!(!selector.is_open);
    }

    #[test]
    fn test_model_selection() {
        // This test would verify model selection behavior
        // let cx = &mut test_context();
        // let mut selector = ModelSelector::new(cx);
        // selector.select_model("Test Model".to_string(), cx);
        // assert_eq!(selector.current_model(), Some(&"Test Model".to_string()));
    }
}
