use gpui::{prelude::*, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    dropdown::{Dropdown, DropdownEvent, DropdownItem, DropdownState, SearchableVec},
    Sizable, Size,
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
    display_info: String,
}

impl ModelItem {
    pub fn new(name: String, display_info: String) -> Self {
        Self { name, display_info }
    }
}

impl DropdownItem for ModelItem {
    type Value = String;

    fn title(&self) -> gpui::SharedString {
        self.name.clone().into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        Some(self.display_info.clone().into_any_element())
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }
}

/// Model selector dropdown component using gpui-component's Dropdown
pub struct ModelSelector {
    dropdown_state: Entity<DropdownState<SearchableVec<ModelItem>>>,
    config: Option<Arc<ConfigurationSystem>>,
    _dropdown_subscription: gpui::Subscription,
}

impl EventEmitter<ModelSelectorEvent> for ModelSelector {}

impl ModelSelector {
    /// Create a new model selector
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dropdown_state =
            cx.new(|cx| DropdownState::new(SearchableVec::new(Vec::new()), None, window, cx));

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

    /// Get model display info (name + provider)
    fn get_model_display_info(config: &ConfigurationSystem, model_name: &str) -> String {
        if let Ok((_model_config, provider_config)) = config.get_model_with_provider(model_name) {
            format!("{} ({})", model_name, provider_config.label)
        } else {
            model_name.to_string()
        }
    }

    /// Handle dropdown events
    fn on_dropdown_event(
        &mut self,
        _: &Entity<DropdownState<SearchableVec<ModelItem>>>,
        event: &DropdownEvent<SearchableVec<ModelItem>>,
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
            let mut models = config.list_models();
            models.sort();

            models
                .into_iter()
                .map(|model_name| {
                    let display_info = Self::get_model_display_info(config, &model_name);
                    ModelItem::new(model_name, display_info)
                })
                .collect()
        } else {
            Vec::new()
        };

        self.dropdown_state.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(model_items), window, cx);
        });

        self.config = config;
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
