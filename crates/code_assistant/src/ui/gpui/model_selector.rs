use gpui::{
    div, prelude::*, AnyElement, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    MouseButton, ParentElement, Render, Styled, Window,
};
use gpui_component::ActiveTheme;
use llm::provider_config::ConfigurationSystem;
use std::sync::Arc;

/// Events emitted by the ModelSelector component
#[derive(Clone, Debug)]
pub enum ModelSelectorEvent {
    /// Model selection changed
    ModelChanged { model_name: String },
    /// Dropdown opened/closed
    DropdownToggled { is_open: bool },
}

/// Model selector dropdown component
pub struct ModelSelector {
    /// Available models loaded from configuration
    models: Vec<String>,
    /// Currently selected model
    current_model: Option<String>,
    /// Whether the dropdown is open
    is_open: bool,
    /// Configuration system for loading model info
    config: Option<Arc<ConfigurationSystem>>,
    /// Focus handle for keyboard navigation
    focus_handle: FocusHandle,
}

impl EventEmitter<ModelSelectorEvent> for ModelSelector {}

impl ModelSelector {
    /// Create a new model selector
    pub fn new(cx: &mut Context<Self>) -> Self {
        let config = ConfigurationSystem::load().ok().map(Arc::new);
        let models = if let Some(ref config) = config {
            let mut models = config.list_models();
            models.sort();
            models
        } else {
            Vec::new()
        };

        Self {
            models,
            current_model: None,
            is_open: false,
            config,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Set the current model
    pub fn set_current_model(&mut self, model_name: Option<String>, cx: &mut Context<Self>) {
        if self.current_model != model_name {
            self.current_model = model_name;
            cx.notify();
        }
    }

    /// Get the current model
    pub fn current_model(&self) -> Option<&String> {
        self.current_model.as_ref()
    }

    /// Toggle the dropdown open/closed
    pub fn toggle_dropdown(&mut self, cx: &mut Context<Self>) {
        self.is_open = !self.is_open;
        cx.emit(ModelSelectorEvent::DropdownToggled {
            is_open: self.is_open,
        });
        cx.notify();
    }

    /// Close the dropdown
    pub fn close_dropdown(&mut self, cx: &mut Context<Self>) {
        if self.is_open {
            self.is_open = false;
            cx.emit(ModelSelectorEvent::DropdownToggled { is_open: false });
            cx.notify();
        }
    }

    /// Select a model
    pub fn select_model(&mut self, model_name: String, cx: &mut Context<Self>) {
        self.current_model = Some(model_name.clone());
        self.is_open = false;

        cx.emit(ModelSelectorEvent::ModelChanged { model_name });
        cx.emit(ModelSelectorEvent::DropdownToggled { is_open: false });
        cx.notify();
    }

    /// Get model display info (name + provider)
    fn get_model_display_info(&self, model_name: &str) -> String {
        if let Some(ref config) = self.config {
            if let Ok((_model_config, provider_config)) = config.get_model_with_provider(model_name)
            {
                format!("{} ({})", model_name, provider_config.label)
            } else {
                model_name.to_string()
            }
        } else {
            model_name.to_string()
        }
    }

    /// Render the dropdown button
    fn render_dropdown_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current_text = match &self.current_model {
            Some(model) => self.get_model_display_info(model),
            None => "Select Model".to_string(),
        };

        let theme = cx.theme();

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(theme.muted))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.toggle_dropdown(cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(current_text),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if self.is_open { "▲" } else { "▼" }),
            )
    }

    /// Render the dropdown menu
    fn render_dropdown_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.is_open {
            return None;
        }

        let theme = cx.theme();

        let menu_items: Vec<AnyElement> = self
            .models
            .iter()
            .map(|model_name| {
                let display_text = self.get_model_display_info(model_name);
                let is_selected = self.current_model.as_ref() == Some(model_name);
                let model_name_clone = model_name.clone();

                div()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .bg(if is_selected {
                        theme.muted
                    } else {
                        theme.background
                    })
                    .hover(|style| style.bg(theme.muted))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.select_model(model_name_clone.clone(), cx);
                        }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(display_text),
                    )
                    .into_any_element()
            })
            .collect();

        Some(
            div()
                .absolute()
                .top_full()
                .left_0()
                .right_0()
                .mt_1()
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .shadow_lg()
                .max_h_64()
                .overflow_y_hidden() // Use available method
                .children(menu_items)
                .into_any_element(),
        )
    }
}

impl Focusable for ModelSelector {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .child(self.render_dropdown_button(cx))
            .children(self.render_dropdown_menu(cx))
    }
}

/// Helper function to create a model selector view
pub fn model_selector(cx: &mut Context<impl Sized>) -> gpui::Entity<ModelSelector> {
    cx.new(ModelSelector::new)
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
