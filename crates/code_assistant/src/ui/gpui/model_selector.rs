use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, Focusable, Render, Window};
use gpui_component::{
    select::{Select, SelectEvent, SelectItem, SelectState},
    ActiveTheme, Icon, Sizable, Size,
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

impl SelectItem for ModelItem {
    type Value = String;

    fn title(&self) -> gpui::SharedString {
        self.name.clone().into()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        let mut row = div().flex().items_center().gap_1().min_w_0();

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
                .child(div().min_w_0().text_ellipsis().child(self.name.clone())),
        );

        Some(row.into_any_element())
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }
}

/// Model selector dropdown component using gpui-component's Dropdown
pub struct ModelSelector {
    dropdown_state: Entity<SelectState<Vec<ModelItem>>>,
    config: Option<Arc<ConfigurationSystem>>,
    _dropdown_subscription: gpui::Subscription,
}

impl EventEmitter<ModelSelectorEvent> for ModelSelector {}

impl ModelSelector {
    /// Create a new model selector
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dropdown_state =
            cx.new(|cx| SelectState::new(Vec::<ModelItem>::new(), None, window, cx));

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

    /// Handle dropdown events
    fn on_dropdown_event(
        &mut self,
        _: &Entity<SelectState<Vec<ModelItem>>>,
        event: &SelectEvent<Vec<ModelItem>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SelectEvent::Confirm(Some(model_name)) => {
                debug!("Model selected: {}", model_name);
                cx.emit(ModelSelectorEvent::ModelChanged {
                    model_name: model_name.clone(),
                });
            }
            SelectEvent::Confirm(None) => {
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
        "ai-core" => Some("icons/ai_sap.svg"),
        "cerebras" => Some("icons/ai_cerebras.svg"),
        "groq" => Some("icons/ai_groq.svg"),
        _ => None,
    }
}

impl Focusable for ModelSelector {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.dropdown_state.focus_handle(cx)
    }
}

impl Render for ModelSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div().text_color(cx.theme().muted_foreground).child(
            Select::new(&self.dropdown_state)
                .placeholder("Select Model")
                .with_size(Size::XSmall)
                .appearance(false)
                .icon(
                    Icon::default()
                        .path("icons/chevron_up_down.svg")
                        .with_size(Size::XSmall)
                        .text_color(cx.theme().muted_foreground),
                )
                .min_w(px(180.))
                .max_w(px(280.)),
        )
    }
}
